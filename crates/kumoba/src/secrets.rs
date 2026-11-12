use crate::{EgressEndpoint, Error, HttpConnectPolicy, Result};
use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::{Arc, RwLock},
};
use zeroize::Zeroizing;

const TOKEN_PREFIX: &str = "kmb_secret_";
const MAX_SECRETS: usize = 1024;
const MIN_SECRET_BYTES: usize = 8;
const MAX_VALUE_BYTES: usize = 4 * 1024 * 1024;
const MAX_BINDINGS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SecretId(uuid::Uuid);

pub struct SecretValue(Zeroizing<String>);

impl SecretValue {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = Zeroizing::new(value.into());
        if !(MIN_SECRET_BYTES..=4096).contains(&value.len())
            || !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
            || value.contains(TOKEN_PREFIX)
        {
            return Err(invalid(
                "secret must contain 8..4096 visible ASCII bytes without reserved tokens",
            ));
        }
        Ok(Self(value))
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SecretMetadata {
    pub id: SecretId,
    pub name: String,
    pub placeholder: String,
    pub revision: u64,
    pub scope: HttpConnectPolicy,
}

struct StoredSecret {
    metadata: SecretMetadata,
    value: Arc<SecretValue>,
}

#[derive(Clone, Default)]
pub struct SecretStore {
    records: Arc<RwLock<BTreeMap<SecretId, Arc<StoredSecret>>>>,
}

impl fmt::Debug for SecretStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretStore([REDACTED])")
    }
}

fn invalid(message: &'static str) -> Error {
    Error::InvalidInput(message.into())
}

fn poisoned() -> Error {
    std::io::Error::other("secret state lock is poisoned").into()
}

fn valid_name(name: &str, hyphen: bool) -> bool {
    (1..=64).contains(&name.len())
        && name
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || hyphen && byte == b'-')
}

fn validate_names(bindings: &BTreeMap<String, SecretId>) -> Result<()> {
    const RESERVED: &[&str] = &[
        "PATH",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
        "NODE_EXTRA_CA_CERTS",
        "AWS_CA_BUNDLE",
        "GIT_SSL_CAINFO",
        "GRPC_DEFAULT_SSL_ROOTS_FILE_PATH",
    ];
    if bindings.len() > MAX_BINDINGS {
        return Err(invalid("at most 32 secret bindings are allowed"));
    }
    for name in bindings.keys() {
        if !valid_name(name, false) || RESERVED.contains(&name.to_ascii_uppercase().as_str()) {
            return Err(invalid("invalid or reserved secret environment name"));
        }
    }
    Ok(())
}

impl SecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(
        &self,
        name: impl Into<String>,
        value: SecretValue,
        scope: HttpConnectPolicy,
    ) -> Result<SecretMetadata> {
        if scope.is_public_web() {
            return Err(invalid("secret scopes require exact endpoints"));
        }
        let name = name.into();
        if !valid_name(&name, true) {
            return Err(invalid("invalid secret name"));
        }
        let mut records = self.records.write().map_err(|_| poisoned())?;
        if records.values().any(|record| record.metadata.name == name) {
            return Err(invalid("secret name already exists"));
        }
        if records.len() >= MAX_SECRETS
            || records
                .values()
                .map(|record| record.value.0.len())
                .sum::<usize>()
                + value.0.len()
                > MAX_VALUE_BYTES
        {
            return Err(invalid("secret store capacity exceeded"));
        }
        let metadata = SecretMetadata {
            id: SecretId(uuid::Uuid::new_v4()),
            name,
            placeholder: format!(
                "{TOKEN_PREFIX}{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
            revision: 1,
            scope,
        };
        records.insert(
            metadata.id,
            Arc::new(StoredSecret {
                metadata: metadata.clone(),
                value: Arc::new(value),
            }),
        );
        Ok(metadata)
    }

    pub fn get(&self, id: SecretId) -> Result<Option<SecretMetadata>> {
        Ok(self
            .records
            .read()
            .map_err(|_| poisoned())?
            .get(&id)
            .map(|record| record.metadata.clone()))
    }

    pub fn list(&self) -> Result<Vec<SecretMetadata>> {
        Ok(self
            .records
            .read()
            .map_err(|_| poisoned())?
            .values()
            .map(|record| record.metadata.clone())
            .collect())
    }

    pub fn rotate(&self, id: SecretId, value: SecretValue) -> Result<SecretMetadata> {
        let mut records = self.records.write().map_err(|_| poisoned())?;
        let current = records
            .get(&id)
            .ok_or_else(|| invalid("secret does not exist"))?;
        if records
            .values()
            .map(|record| record.value.0.len())
            .sum::<usize>()
            - current.value.0.len()
            + value.0.len()
            > MAX_VALUE_BYTES
        {
            return Err(invalid("secret store capacity exceeded"));
        }
        let mut metadata = current.metadata.clone();
        metadata.revision = metadata
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("secret revision exhausted"))?;
        records.insert(
            id,
            Arc::new(StoredSecret {
                metadata: metadata.clone(),
                value: Arc::new(value),
            }),
        );
        Ok(metadata)
    }

    pub fn update_scope(&self, id: SecretId, scope: HttpConnectPolicy) -> Result<SecretMetadata> {
        if scope.is_public_web() {
            return Err(invalid("secret scopes require exact endpoints"));
        }
        let mut records = self.records.write().map_err(|_| poisoned())?;
        let current = records
            .get(&id)
            .ok_or_else(|| invalid("secret does not exist"))?;
        let mut metadata = current.metadata.clone();
        metadata.revision = metadata
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("secret revision exhausted"))?;
        metadata.scope = scope;
        let value = current.value.clone();
        records.insert(
            id,
            Arc::new(StoredSecret {
                metadata: metadata.clone(),
                value,
            }),
        );
        Ok(metadata)
    }

    pub fn delete(&self, id: SecretId) -> Result<bool> {
        Ok(self
            .records
            .write()
            .map_err(|_| poisoned())?
            .remove(&id)
            .is_some())
    }

    pub fn validate_bindings(
        &self,
        bindings: &BTreeMap<String, SecretId>,
        policy: &HttpConnectPolicy,
    ) -> Result<()> {
        validate_names(bindings)?;
        let records = self.records.read().map_err(|_| poisoned())?;
        for id in bindings.values() {
            let record = records
                .get(id)
                .ok_or_else(|| invalid("secret binding refers to a missing secret"))?;
            if !record
                .metadata
                .scope
                .endpoints()
                .iter()
                .any(|endpoint| policy.allows(endpoint))
            {
                return Err(invalid(
                    "secret has no destination permitted by sandbox policy",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct LiveBindings(Arc<RwLock<BTreeMap<String, SecretId>>>);

impl LiveBindings {
    pub(crate) fn new(bindings: BTreeMap<String, SecretId>) -> Self {
        Self(Arc::new(RwLock::new(bindings)))
    }

    pub(crate) fn replace(&self, bindings: BTreeMap<String, SecretId>) -> Result<()> {
        validate_names(&bindings)?;
        *self.0.write().map_err(|_| poisoned())? = bindings;
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct SecretSession {
    store: SecretStore,
    bindings: LiveBindings,
}

impl SecretSession {
    pub(crate) fn new(store: SecretStore, bindings: LiveBindings) -> Self {
        Self { store, bindings }
    }

    pub(crate) fn environment(&self) -> Result<BTreeMap<String, String>> {
        let bindings = self.bindings.0.read().map_err(|_| poisoned())?;
        validate_names(&bindings)?;
        let records = self.store.records.read().map_err(|_| poisoned())?;
        Ok(bindings
            .iter()
            .filter_map(|(name, id)| {
                records
                    .get(id)
                    .map(|record| (name.clone(), record.metadata.placeholder.clone()))
            })
            .collect())
    }

    pub(crate) fn request(&self, endpoint: &EgressEndpoint) -> Result<RequestSecrets> {
        let bindings = self.bindings.0.read().map_err(|_| poisoned())?;
        validate_names(&bindings)?;
        let records = self.store.records.read().map_err(|_| poisoned())?;
        let bound: Vec<_> = bindings
            .values()
            .copied()
            .collect::<BTreeSet<_>>()
            .iter()
            .filter_map(|id| records.get(id).cloned())
            .collect();
        drop(records);
        drop(bindings);
        let authorized: Vec<_> = bound
            .iter()
            .filter(|record| record.metadata.scope.allows(endpoint))
            .cloned()
            .collect();
        let substitutions = matcher(
            authorized
                .iter()
                .map(|record| record.metadata.placeholder.as_bytes()),
        )?;
        let scrubber = matcher(bound.iter().map(|record| record.value.0.as_bytes()))?;
        Ok(RequestSecrets {
            bound,
            authorized,
            substitutions,
            scrubber,
        })
    }
}

fn matcher<'a>(patterns: impl IntoIterator<Item = &'a [u8]>) -> Result<Option<AhoCorasick>> {
    let patterns: Vec<_> = patterns.into_iter().collect();
    if patterns.is_empty() {
        return Ok(None);
    }
    AhoCorasickBuilder::new()
        .match_kind(MatchKind::LeftmostLongest)
        .build(patterns)
        .map(Some)
        .map_err(|_| invalid("secret matcher capacity exceeded"))
}

pub(crate) struct RequestSecrets {
    bound: Vec<Arc<StoredSecret>>,
    authorized: Vec<Arc<StoredSecret>>,
    substitutions: Option<AhoCorasick>,
    scrubber: Option<AhoCorasick>,
}

impl RequestSecrets {
    pub(crate) fn authorizes_any(&self) -> bool {
        !self.authorized.is_empty()
    }

    pub(crate) fn substitute(&self, bytes: &[u8], limit: usize) -> Result<Vec<u8>> {
        transform(bytes, limit, self.substitutions.as_ref(), |index| {
            self.authorized[index].value.0.as_bytes()
        })
    }

    pub(crate) fn scrub(&self, bytes: &[u8], limit: usize) -> Result<Vec<u8>> {
        transform(bytes, limit, self.scrubber.as_ref(), |index| {
            self.bound[index].metadata.placeholder.as_bytes()
        })
    }
}

pub(crate) struct StreamScrubber {
    secrets: Arc<RequestSecrets>,
    pending: Zeroizing<Vec<u8>>,
    prefixes: Vec<Vec<usize>>,
}

impl StreamScrubber {
    pub(crate) fn new(secrets: Arc<RequestSecrets>) -> Self {
        let prefixes = secrets
            .bound
            .iter()
            .map(|record| {
                let value = record.value.0.as_bytes();
                let mut table = vec![0; value.len()];
                for i in 1..value.len() {
                    let mut matched = table[i - 1];
                    while matched > 0 && value[i] != value[matched] {
                        matched = table[matched - 1];
                    }
                    if value[i] == value[matched] {
                        matched += 1;
                    }
                    table[i] = matched;
                }
                table
            })
            .collect();
        Self {
            secrets,
            pending: Zeroizing::new(Vec::new()),
            prefixes,
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8], limit: usize) -> Result<Vec<u8>> {
        if chunk.len() > 16 * 1024 {
            return Err(invalid("secret stream chunk exceeds limit"));
        }
        self.pending.extend_from_slice(chunk);
        let input = &self.pending;
        let mut retained = 0;
        for (record, table) in self.secrets.bound.iter().zip(&self.prefixes) {
            let pattern = record.value.0.as_bytes();
            let mut matched = 0;
            for byte in input.iter() {
                while matched > 0 && *byte != pattern[matched] {
                    matched = table[matched - 1];
                }
                if *byte == pattern[matched] {
                    matched += 1;
                }
                if matched == pattern.len() {
                    matched = table[matched - 1];
                }
            }
            retained = retained.max(matched);
        }
        let boundary = input.len() - retained;
        let mut output = Vec::new();
        let mut append = |bytes: &[u8]| -> Result<()> {
            if bytes.len() > limit.saturating_sub(output.len()) {
                return Err(invalid("secret stream output exceeds limit"));
            }
            output.extend_from_slice(bytes);
            Ok(())
        };
        let mut cursor = 0;
        if let Some(matcher) = &self.secrets.scrubber {
            for found in matcher.find_iter(input) {
                if found.start() >= boundary {
                    break;
                }
                append(&input[cursor..found.start()])?;
                append(
                    self.secrets.bound[found.pattern().as_usize()]
                        .metadata
                        .placeholder
                        .as_bytes(),
                )?;
                cursor = found.end();
            }
        }
        let consumed = boundary.max(cursor);
        append(&input[cursor..consumed])?;
        self.pending = Zeroizing::new(input[consumed..].to_vec());
        Ok(output)
    }

    pub(crate) fn finish(self, limit: usize) -> Result<Vec<u8>> {
        self.secrets.scrub(&self.pending, limit)
    }
}

fn transform<'a>(
    bytes: &[u8],
    limit: usize,
    matcher: Option<&AhoCorasick>,
    replacement: impl Fn(usize) -> &'a [u8],
) -> Result<Vec<u8>> {
    let mut output = Vec::with_capacity(bytes.len().min(limit));
    let mut append = |part: &[u8]| -> Result<()> {
        if part.len() > limit.saturating_sub(output.len()) {
            return Err(invalid("secret transformation exceeds output limit"));
        }
        output.extend_from_slice(part);
        Ok(())
    };
    let mut cursor = 0;
    if let Some(matcher) = matcher {
        for matched in matcher.find_iter(bytes) {
            append(&bytes[cursor..matched.start()])?;
            append(replacement(matched.pattern().as_usize()))?;
            cursor = matched.end();
        }
    }
    append(&bytes[cursor..])?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(host: &str) -> EgressEndpoint {
        EgressEndpoint::new(host, 443).unwrap()
    }
    fn policy(host: &str) -> HttpConnectPolicy {
        HttpConnectPolicy::new([endpoint(host)]).unwrap()
    }
    fn value(text: &str) -> SecretValue {
        SecretValue::new(text).unwrap()
    }
    fn bindings(id: SecretId) -> BTreeMap<String, SecretId> {
        BTreeMap::from([("API_KEY".into(), id)])
    }
    const OLD: &str = "old-credential-0123456789";
    const NEW: &str = "new-credential-0123456789";

    #[test]
    fn metadata_and_debug_never_expose_values() {
        let store = SecretStore::new();
        let secret = store
            .create("api-key", value(OLD), policy("api.example"))
            .unwrap();
        for debug in [
            format!("{store:?}"),
            format!("{:?}", value(OLD)),
            format!("{secret:?}"),
            format!("{:?}", store.list().unwrap()),
        ] {
            assert!(!debug.contains(OLD));
        }
        assert_eq!(store.get(secret.id).unwrap().unwrap().name, "api-key");
        assert!(!serde_json::to_string(&secret).unwrap().contains(OLD));
        assert_eq!(secret.placeholder.len(), TOKEN_PREFIX.len() + 64);
        assert!(
            store
                .create("api-key", value(NEW), policy("api.example"))
                .is_err()
        );
        assert!(SecretValue::new("eight-ch").is_ok());
        for bad in [
            "short",
            "sixteen-bytes\r\nx",
            "kmb_secret_forbidden01234",
            "a-tab-is-control\tvalue",
            "éééééééééééééééé",
        ] {
            assert!(SecretValue::new(bad).is_err());
        }
        assert!(SecretValue::new("x".repeat(4097)).is_err());
        assert!(SecretValue::new("x".repeat(4096)).is_ok());
        assert!(SecretValue::new("Bearer credential-value").is_ok());
    }

    #[test]
    fn tokens_require_this_sandbox_binding_and_exact_scope() {
        let store = SecretStore::new();
        let secret = store
            .create("key", value(OLD), policy("api.example"))
            .unwrap();
        let session = SecretSession::new(store.clone(), LiveBindings::new(bindings(secret.id)));
        let other = SecretSession::new(store.clone(), LiveBindings::new(BTreeMap::new()));
        let token = secret.placeholder.as_bytes();
        assert_eq!(
            session
                .request(&endpoint("api.example"))
                .unwrap()
                .substitute(token, 4096)
                .unwrap(),
            OLD.as_bytes()
        );
        for wrong in [
            endpoint("other.api.example"),
            EgressEndpoint::new("api.example", 8443).unwrap(),
        ] {
            let request = session.request(&wrong).unwrap();
            assert_eq!(request.substitute(token, 4096).unwrap(), token);
            assert_eq!(request.scrub(OLD.as_bytes(), 4096).unwrap(), token);
        }
        assert_eq!(
            other
                .request(&endpoint("api.example"))
                .unwrap()
                .substitute(token, 4096)
                .unwrap(),
            token
        );
        assert!(
            store
                .validate_bindings(&bindings(secret.id), &policy("api.example"))
                .is_ok()
        );
        assert!(
            store
                .validate_bindings(&bindings(secret.id), &policy("other.example"))
                .is_err()
        );
    }

    #[test]
    fn admitted_requests_keep_old_versions_after_rotation_deletion_and_detach() {
        let store = SecretStore::new();
        let secret = store
            .create("key", value(OLD), policy("api.example"))
            .unwrap();
        let live = LiveBindings::new(bindings(secret.id));
        let session = SecretSession::new(store.clone(), live.clone());
        let old = session.request(&endpoint("api.example")).unwrap();
        let rotated = store.rotate(secret.id, value(NEW)).unwrap();
        assert_eq!(secret.placeholder, rotated.placeholder);
        assert_eq!(rotated.revision, 2);
        let token = secret.placeholder.as_bytes();
        let new = session.request(&endpoint("api.example")).unwrap();
        assert_eq!(old.substitute(token, 4096).unwrap(), OLD.as_bytes());
        assert_eq!(new.substitute(token, 4096).unwrap(), NEW.as_bytes());
        store
            .update_scope(secret.id, policy("other.example"))
            .unwrap();
        assert_eq!(new.substitute(token, 4096).unwrap(), NEW.as_bytes());
        assert_eq!(
            session
                .request(&endpoint("api.example"))
                .unwrap()
                .substitute(token, 4096)
                .unwrap(),
            token
        );
        live.replace(BTreeMap::new()).unwrap();
        assert!(session.environment().unwrap().is_empty());
        assert_eq!(
            session
                .request(&endpoint("api.example"))
                .unwrap()
                .substitute(token, 4096)
                .unwrap(),
            token
        );
        live.replace(bindings(secret.id)).unwrap();
        assert_eq!(
            session.environment().unwrap().get("API_KEY"),
            Some(&secret.placeholder)
        );
        assert!(store.delete(secret.id).unwrap());
        assert!(!store.delete(secret.id).unwrap());
        assert!(session.environment().unwrap().is_empty());
        assert!(
            store
                .validate_bindings(&bindings(secret.id), &policy("api.example"))
                .is_err()
        );
        let recreated = store
            .create("key", value(NEW), policy("api.example"))
            .unwrap();
        assert_ne!(secret.id, recreated.id);
        assert_ne!(secret.placeholder, recreated.placeholder);
        assert!(session.environment().unwrap().is_empty());
        live.replace(BTreeMap::new()).unwrap();
        assert_eq!(old.scrub(OLD.as_bytes(), 4096).unwrap(), token);
        assert_eq!(new.scrub(NEW.as_bytes(), 4096).unwrap(), token);
        assert_eq!(
            session
                .request(&endpoint("api.example"))
                .unwrap()
                .substitute(token, 4096)
                .unwrap(),
            token
        );
    }

    #[test]
    fn longest_match_is_single_pass_and_expansion_is_bounded() {
        let store = SecretStore::new();
        let short = store
            .create("short", value("0123456789abcdef"), policy("api.example"))
            .unwrap();
        let long = store
            .create(
                "long",
                value("0123456789abcdef-long"),
                policy("api.example"),
            )
            .unwrap();
        let live = LiveBindings::new(BTreeMap::from([
            ("SHORT".into(), short.id),
            ("LONG".into(), long.id),
        ]));
        let request = SecretSession::new(store, live)
            .request(&endpoint("api.example"))
            .unwrap();
        let input = b"a0123456789abcdef-longb0123456789abcdef";
        let expected = format!("a{}b{}", long.placeholder, short.placeholder);
        assert_eq!(
            request.scrub(input, expected.len()).unwrap(),
            expected.as_bytes()
        );
        assert!(request.scrub(input, expected.len() - 1).is_err());
        assert_eq!(
            request
                .substitute(expected.as_bytes(), input.len())
                .unwrap(),
            input
        );
        assert!(
            request
                .substitute(expected.as_bytes(), input.len() - 1)
                .is_err()
        );
        assert_eq!(request.scrub(b"", 0).unwrap(), b"");
        assert!(request.scrub(b"x", 0).is_err());
        let untouched = b"kmb_secret_unknown-token0000000000000000";
        assert_eq!(request.substitute(untouched, 100).unwrap(), untouched);
    }

    #[test]
    fn bindings_validate_reserved_names_and_capacity() {
        let store = SecretStore::new();
        let secret = store
            .create("key", value(OLD), policy("api.example"))
            .unwrap();
        for name in [
            "PATH",
            "HTTPS_PROXY",
            "https_proxy",
            "SSL_CERT_FILE",
            "NODE_EXTRA_CA_CERTS",
            "9KEY",
            "KEY-BAD",
            "",
        ] {
            assert!(
                store
                    .validate_bindings(
                        &BTreeMap::from([(name.into(), secret.id)]),
                        &policy("api.example")
                    )
                    .is_err()
            );
        }
        let excessive = (0..33)
            .map(|index| (format!("KEY_{index}"), secret.id))
            .collect();
        assert!(
            LiveBindings::new(BTreeMap::new())
                .replace(excessive)
                .is_err()
        );
        for index in 1..MAX_SECRETS {
            store
                .create(format!("key_{index}"), value(OLD), policy("api.example"))
                .unwrap();
        }
        assert!(
            store
                .create("overflow", value(OLD), policy("api.example"))
                .is_err()
        );
    }

    #[test]
    fn poisoned_store_returns_error_instead_of_panicking() {
        let store = SecretStore::new();
        let cloned = store.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = cloned.records.write().unwrap();
                panic!("test lock poisoning");
            })
            .join()
            .is_err()
        );
        assert!(store.list().is_err());
        assert!(
            store
                .create("key", value(OLD), policy("api.example"))
                .is_err()
        );
    }
    #[test]
    fn streamed_redaction_is_independent_of_fragment_boundaries() {
        let store = SecretStore::new();
        let patterns = [
            "abcdefghijklmnop",
            "abcdefghijklmnop-more",
            "ijklmnop-moreabcdefghijkl",
            "aaaaaaaaaaaaaaaa",
            "aaaaaaaabaaaaaaaab",
        ];
        let mut refs = BTreeMap::new();
        for (index, pattern) in patterns.iter().enumerate() {
            let secret = store
                .create(
                    format!("key_{index}"),
                    value(pattern),
                    policy("api.example"),
                )
                .unwrap();
            refs.insert(format!("KEY_{index}"), secret.id);
        }
        let secrets = Arc::new(
            SecretSession::new(store, LiveBindings::new(refs))
                .request(&endpoint("api.example"))
                .unwrap(),
        );
        let inputs = [
            "data: ordinary token\n\n",
            "prefixabcdefghijklmnop-moreabcdefghijklmnopend",
            "abcdefghijklmnop-moreabcdefghijklxxxxxxxxijklmnop-moreabcdefghijkl",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaabaaaaaaaab-end",
            "ends-with-incomplete-abcdefghijk",
        ];
        for input in inputs {
            let expected = secrets.scrub(input.as_bytes(), 8192).unwrap();
            for first in 0..=input.len() {
                for second in first..=input.len() {
                    let mut stream = StreamScrubber::new(secrets.clone());
                    let mut actual = Vec::new();
                    for part in [
                        &input.as_bytes()[..first],
                        &input.as_bytes()[first..second],
                        &input.as_bytes()[second..],
                    ] {
                        actual.extend(stream.push(part, 8192).unwrap());
                        assert!(
                            stream.pending.len() < patterns.iter().map(|p| p.len()).max().unwrap()
                        );
                    }
                    actual.extend(stream.finish(8192).unwrap());
                    assert_eq!(actual, expected, "split {first}/{second}");
                }
            }
        }
        let mut stream = StreamScrubber::new(secrets);
        assert_eq!(
            stream.push(b"data: ordinary token\n\n", 8192).unwrap(),
            b"data: ordinary token\n\n"
        );
        assert!(stream.push(b"abcdefghijkl", 8192).unwrap().is_empty());
        assert!(stream.push(b"mnop", 1).unwrap().is_empty()); // longer value remains possible
        assert!(stream.finish(1).is_err());
    }
}

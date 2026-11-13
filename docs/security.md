# Security model

- **VM isolation.** Each sandbox runs in a hardware-isolated VM with its own kernel. Its only channels to the host are the guest agent and the egress proxy.
- **Network.** Private, link-local and cloud metadata addresses are never reachable, whatever the sandbox's policy. An operator can open specific intranet hosts with `KUMOBAD_ALLOW_PRIVATE_HOSTS`; loopback, link-local and the host's own addresses stay blocked even then.
- **Corporate proxies.** With `KUMOBAD_UPSTREAM_PROXY`, kumobad resolves and checks each destination itself before tunnelling through the proxy. The guest's TLS session runs inside the tunnel.
- **Secrets.** Values stay on the host. The guest only holds a placeholder, and the real value goes only into HTTPS requests to the hosts you allow. See [How secrets work](/secrets).
- **Previews.** Links are signed, bound to one sandbox and port, and expire. Browsers load them from a separate listener (`127.0.0.1:7789`) that serves nothing else, so a page from a sandbox can't reach the API. Redirects stay inside the preview.
- **Known limit.** All previews share one browser origin, so browser storage such as `localStorage` is shared between sandboxes' pages. Don't open untrusted previews in a browser profile whose preview storage matters. Per-sandbox origins are planned.
- **Daemon access.** The Unix socket is mode `0600`, and the TCP listeners accept loopback connections only.

<!--@include: ../SECURITY.md{3,}-->

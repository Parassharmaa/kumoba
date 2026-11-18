use kumoba_protocol::{Response, desktop::DesktopRequest};
use std::io;

pub(crate) fn available() -> bool {
    false
}

#[derive(Default)]
pub(crate) struct Desktop;

impl Desktop {
    pub(crate) fn inherit_environment(
        &mut self,
        _env: &mut std::collections::BTreeMap<String, String>,
    ) {
    }

    pub(crate) async fn request(&mut self, _request: DesktopRequest) -> io::Result<Response> {
        Ok(Response::Error(
            "this sandbox has no desktop service".into(),
        ))
    }
}

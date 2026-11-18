use crate::cli::{CliError, CliResult};

/// Exchange exactly one request. Retry, authentication, and redirect decisions
/// belong to the service calling this trait, not the network adapter.
pub(crate) trait Transport {
    fn send(&mut self, request: Request) -> CliResult<Response>;
}

pub(crate) struct Request {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub(crate) struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub(crate) struct Offline;

impl Transport for Offline {
    fn send(&mut self, _: Request) -> CliResult<Response> {
        Err(CliError::new(
            4,
            "network",
            "HTTP transport is not connected in this migration build",
            "",
        ))
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::collections::VecDeque;

    /// Records every attempt before matching a response. No real fallback.
    #[derive(Default)]
    pub(crate) struct Scripted {
        pub expected: VecDeque<(&'static str, String, CliResult<Response>)>,
        pub calls: Vec<Request>,
    }

    impl Transport for Scripted {
        fn send(&mut self, request: Request) -> CliResult<Response> {
            self.calls.push(request);
            let Some((method, url, response)) = self.expected.pop_front() else {
                return Err(unexpected());
            };
            let request = self.calls.last().expect("recorded request");
            if request.method != method || request.url != url {
                return Err(unexpected());
            }
            response
        }
    }

    fn unexpected() -> CliError {
        CliError::new(5, "upstream", "unexpected request in offline test", "")
    }
}

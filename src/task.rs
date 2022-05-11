use crate::client::SlowClient;
use futures::future::FutureExt;

pub enum RequestTask<'a, T> {
    Working(tokio::task::JoinHandle<(T, SlowClient<'a>)>),
    Waiting(Option<SlowClient<'a>>),
}

impl<'a, T> RequestTask<'a, T> {
    pub fn new(client: SlowClient<'a>) -> Self {
        RequestTask::Waiting(Some(client))
    }

    pub fn try_start<F>(&mut self, f: F)
    where
        F: FnOnce(
            SlowClient<'a>,
        ) -> Result<tokio::task::JoinHandle<(T, SlowClient<'a>)>, SlowClient<'a>>,
    {
        if let RequestTask::Waiting(client) = self {
            let client_ = client
                .take()
                .expect("`RequestTask` client invariant failed in `try_start`");
            match f(client_) {
                Ok(handle) => *self = RequestTask::Working(handle),
                Err(c) => *self = RequestTask::Waiting(Some(c)),
            }
        }
    }

    pub fn try_finish(&mut self) -> Option<T> {
        match self {
            RequestTask::Working(handle) => match handle.now_or_never() {
                Some(Ok((x, client))) => {
                    *self = RequestTask::Waiting(Some(client));
                    Some(x)
                }
                Some(Err(e)) => panic!("`RequestTask` failed: {}", e),
                None => None,
            },
            RequestTask::Waiting(_) => None,
        }
    }

    pub fn is_working_or<F>(&self, f: F) -> bool
    where
        F: Fn(&SlowClient<'a>) -> bool,
    {
        match self {
            RequestTask::Working(_) => true,
            RequestTask::Waiting(client) => f(client
                .as_ref()
                .expect("`RequestTask` client invariant failed in `is_working_or`")),
        }
    }

    pub fn is_waiting(&self) -> bool {
        match self {
            RequestTask::Working(_) => false,
            RequestTask::Waiting(_) => true,
        }
    }
}

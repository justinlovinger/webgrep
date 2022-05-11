use futures::future::FutureExt;

pub enum TaskWithResource<T, R> {
    Working(tokio::task::JoinHandle<(T, R)>),
    Waiting(Option<R>),
}

impl<T, R> TaskWithResource<T, R> {
    pub fn new(r: R) -> Self {
        TaskWithResource::Waiting(Some(r))
    }

    pub fn try_start<F>(&mut self, f: F)
    where
        F: FnOnce(R) -> Result<tokio::task::JoinHandle<(T, R)>, R>,
    {
        if let TaskWithResource::Waiting(r) = self {
            let r_ = r
                .take()
                .expect("`TaskWithResource` resource invariant failed in `try_start`");
            match f(r_) {
                Ok(handle) => *self = TaskWithResource::Working(handle),
                Err(c) => *self = TaskWithResource::Waiting(Some(c)),
            }
        }
    }

    pub fn try_finish(&mut self) -> Option<T> {
        match self {
            TaskWithResource::Working(handle) => match handle.now_or_never() {
                Some(Ok((x, r))) => {
                    *self = TaskWithResource::Waiting(Some(r));
                    Some(x)
                }
                Some(Err(e)) => panic!("`TaskWithResource` failed: {}", e),
                None => None,
            },
            TaskWithResource::Waiting(_) => None,
        }
    }

    pub fn is_working_or<F>(&self, f: F) -> bool
    where
        F: Fn(&R) -> bool,
    {
        match self {
            TaskWithResource::Working(_) => true,
            TaskWithResource::Waiting(r) => f(r
                .as_ref()
                .expect("`TaskWithResource` resource invariant failed in `is_working_or`")),
        }
    }

    pub fn is_waiting(&self) -> bool {
        match self {
            TaskWithResource::Working(_) => false,
            TaskWithResource::Waiting(_) => true,
        }
    }
}

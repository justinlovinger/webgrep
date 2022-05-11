use futures::future::FutureExt;

pub struct TaskWithResource<T, R>(Option<InnerTaskWithResource<T, R>>);

impl<T, R> TaskWithResource<T, R> {
    pub fn new(r: R) -> Self {
        Self(Some(InnerTaskWithResource::Waiting(r)))
    }

    pub fn try_start<F>(&mut self, f: F)
    where
        F: FnOnce(R) -> Result<tokio::task::JoinHandle<(T, R)>, R>,
    {
        match self.0.take() {
            Some(InnerTaskWithResource::Working(handle)) => {
                self.0 = Some(InnerTaskWithResource::Working(handle))
            }
            Some(InnerTaskWithResource::Waiting(r)) => match f(r) {
                Ok(handle) => self.0 = Some(InnerTaskWithResource::Working(handle)),
                Err(r_) => self.0 = Some(InnerTaskWithResource::Waiting(r_)),
            },
            None => panic!("Resource invariant failed in `TaskWithResource::try_start`"),
        }
    }

    pub fn try_finish(&mut self) -> Option<T> {
        match &mut self.0 {
            Some(InnerTaskWithResource::Working(handle)) => match handle.now_or_never() {
                Some(Ok((x, r))) => {
                    self.0 = Some(InnerTaskWithResource::Waiting(r));
                    Some(x)
                }
                Some(Err(e)) => panic!("`TaskWithResource` failed: {}", e),
                None => None,
            },
            Some(InnerTaskWithResource::Waiting(_)) => None,
            None => panic!("Resource invariant failed in `TaskWithResource::try_finish`"),
        }
    }

    pub fn is_working_or<F>(&self, f: F) -> bool
    where
        F: Fn(&R) -> bool,
    {
        match &self.0 {
            Some(InnerTaskWithResource::Working(_)) => true,
            Some(InnerTaskWithResource::Waiting(r)) => f(r),
            None => panic!("Resource invariant failed in `TaskWithResource::is_working_or`"),
        }
    }

    pub fn is_waiting(&self) -> bool {
        match &self.0 {
            Some(InnerTaskWithResource::Working(_)) => false,
            Some(InnerTaskWithResource::Waiting(_)) => true,
            None => panic!("Resource invariant failed in `TaskWithResource::is_waiting`"),
        }
    }
}

enum InnerTaskWithResource<T, R> {
    Working(tokio::task::JoinHandle<(T, R)>),
    Waiting(R),
}

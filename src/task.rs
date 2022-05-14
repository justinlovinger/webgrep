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
        debug_assert!(
            self.0.is_some(),
            "Resource invariant failed in `TaskWithResource::try_start`"
        );
        match unsafe { self.0.take().unwrap_unchecked() } {
            InnerTaskWithResource::Working(handle) => {
                self.0 = Some(InnerTaskWithResource::Working(handle))
            }
            InnerTaskWithResource::Waiting(r) => match f(r) {
                Ok(handle) => self.0 = Some(InnerTaskWithResource::Working(handle)),
                Err(r_) => self.0 = Some(InnerTaskWithResource::Waiting(r_)),
            },
        }
    }

    pub fn try_finish(&mut self) -> Option<T> {
        debug_assert!(
            self.0.is_some(),
            "Resource invariant failed in `TaskWithResource::try_finish`"
        );
        match unsafe { self.0.as_mut().unwrap_unchecked() } {
            InnerTaskWithResource::Working(handle) => match handle.now_or_never() {
                Some(Ok((x, r))) => {
                    self.0 = Some(InnerTaskWithResource::Waiting(r));
                    Some(x)
                }
                Some(Err(e)) => panic!("`TaskWithResource` failed: {}", e),
                None => None,
            },
            InnerTaskWithResource::Waiting(_) => None,
        }
    }

    pub fn is_working_or<F>(&self, f: F) -> bool
    where
        F: Fn(&R) -> bool,
    {
        debug_assert!(
            self.0.is_some(),
            "Resource invariant failed in `TaskWithResource::is_working_or`"
        );
        match unsafe { &self.0.as_ref().unwrap_unchecked() } {
            InnerTaskWithResource::Working(_) => true,
            InnerTaskWithResource::Waiting(r) => f(r),
        }
    }

    pub fn is_waiting(&self) -> bool {
        debug_assert!(
            self.0.is_some(),
            "Resource invariant failed in `TaskWithResource::is_waiting`"
        );
        match unsafe { self.0.as_ref().unwrap_unchecked() } {
            InnerTaskWithResource::Working(_) => false,
            InnerTaskWithResource::Waiting(_) => true,
        }
    }
}

enum InnerTaskWithResource<T, R> {
    Working(tokio::task::JoinHandle<(T, R)>),
    Waiting(R),
}

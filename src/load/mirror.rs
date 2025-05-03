use std::{future::Future, ops::Deref};

pub struct LoadMirror;

impl<P> super::Layer2<P> for LoadMirror
where
    P: Deref,
    P::Target: crate::Value,
{
    type Incomplete = ();
    type Complete = ();

    fn insert(&self, write: impl super::Write<P, Self::Complete>) -> P {
        write.write(())
    }

    fn read(&self, _pointer: &P, _complete: &Self::Complete) -> super::ReadResult {
        super::ReadResult::BackgroundRefresh
    }

    fn start(&self, _key: &<P::Target as crate::Value>::Key) -> Option<Self::Incomplete> {
        None
    }

    fn complete(
        &self,
        _incomplete: &Self::Incomplete,
        write: impl super::Write<P, Self::Complete>,
    ) -> P {
        write.write(())
    }

    fn notify(&self, _incomplete: &Self::Incomplete, _pointer: &P, _complete: &Self::Complete) {}

    fn wait(&self, _incomplete: &Self::Incomplete) -> impl Future<Output = P> + Send {
        unreachable!();
        #[allow(unreachable_code)]
        std::future::pending()
    }
}

use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::heap::Alloc;
use super::inner::PoolAllocInner;

#[derive(Debug)]
pub struct Ref<'d, A: Alloc> {
    // Needed so we can remove from the pool on drop
    pub(crate) allocator: Rc<PoolAllocInner<A>>,
    pub(crate) data: &'d mut [u8],
}

impl<'d, A: Alloc> Ref<'d, A> {
    pub(crate) fn from(allocator: &Rc<PoolAllocInner<A>>, data: &'d mut [u8]) -> Self {
        Ref { allocator: allocator.clone(), data }
    }
}

impl<'d, A: Alloc> Deref for Ref<'d, A> {
    type Target = [u8];

    fn deref(&self) -> &[u8] { self.data }
}

impl<'d, A: Alloc> DerefMut for Ref<'d, A> {
    fn deref_mut(&mut self) -> &mut [u8] { self.data }
}

impl<'d, A: Alloc> Drop for Ref<'d, A> {
    fn drop(&mut self) {
        self.allocator.free(self.data);
    }
}
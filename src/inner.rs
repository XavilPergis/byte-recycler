use std::collections::BTreeMap;
use std::heap::{Alloc, AllocErr, Heap};
use std::ops::{Deref, DerefMut};
use std::ptr;
use std::sync::RwLock;
use std::rc::Rc;
use std::mem;
use std::sync::RwLockReadGuard;

use super::owned_ref::Ref;

#[derive(Debug)]
struct Entry {
    occupied: bool,
    item: *mut [u8],
}

#[derive(Debug)]
pub(crate) struct Bucket<A: Alloc> {
    allocator: Rc<PoolAllocInner<A>>,
    bucket: RwLock<Vec<Entry>>,
}

impl<A: Alloc> Bucket<A> {
    fn new(allocator: Rc<PoolAllocInner<A>>) -> Self {
        Bucket {
            // Clone the `Rc`
            allocator: allocator.clone(),
            bucket: RwLock::new(Vec::new())
        }
    }

    fn push_entry(&self, item: *mut [u8]) {
        let mut bucket = self.bucket.write().unwrap();
        // Pushing a new item with a value will make the entry occupied! (duh)
        bucket.push(Entry { occupied: true, item });
    }
    
    fn is_full(&self) -> bool {
        // we have to be the only one with access to the vec
        // because we can't have the bools changing under our feet!
        // We only need the weakest guarantee, because 
        self.bucket.write().unwrap().iter().all(|entry| entry.occupied)
    }

    // Find the first available slot and mark it
    fn get_and_mark<'a, 'd: 'a>(&'d self, allocator: Rc<PoolAllocInner<A>>) -> Option<Ref<'d, A>> {
        let mut bucket = self.bucket.write().unwrap();

        // Filter out all the occupied buckets and get the next item, which will
        // NOT be occupied.
        if let Some(entry) = bucket.iter_mut().filter(|entry| !entry.occupied).next() {
            // Then set it to be occupied...
            entry.occupied = true;

            // And return the ref!
            return Some(Ref {
                allocator: allocator.clone(),
                // Since we only ever return refs that are marked as open, and we
                // only open entries when a reference is dropped, we satisfy the
                // aliasing invariant of `&mut`
                data: unsafe { mem::transmute::<_, &'d mut [u8]>(entry.item) }
            });
        }

        None
    }
}

#[derive(Debug)]
pub struct PoolAllocInner<A: Alloc = Heap> {
    allocator: RwLock<A>,
    buckets: RwLock<BTreeMap<usize, Bucket<A>>>,
}

#[derive(Clone, Debug)]
pub struct PoolAlloc<A: Alloc = Heap>(Rc<PoolAllocInner<A>>);

impl<A: Alloc> Deref for PoolAlloc<A> {
    type Target = PoolAllocInner<A>;

    fn deref(&self) -> &Self::Target { self.0.deref() }
}

unsafe fn alloc_slice<A: Alloc>(len: usize, allocator: &mut A) -> Result<*mut [u8], AllocErr> {
    let mut ptr = allocator.alloc_array::<u8>(len)?;
    Ok(::std::slice::from_raw_parts_mut(ptr.as_ptr(), len))
}

impl<A: Alloc> PoolAllocInner<A> {
    pub(crate) fn free(&self, item: *mut [u8]) {
        // Search through every entry in all the buckets
        for bucket in self.buckets.read().unwrap().values() {
            if let Some(entry) = bucket.bucket.write().unwrap().iter_mut().filter(|entry| entry.item == item).next() {
                entry.occupied = false;
                return;
            }
        }
    }
}

impl<A: Alloc> PoolAlloc<A> {
    // /// Construct a new pool allocator with the default Rust allocator
    // pub fn new() -> Self {
    //     PoolAlloc { allocator: Heap, buckets: RwLock::new(BTreeMap::new()) }
    // }

    /// Construct a new pool allocator, using a custom backing allocator
    pub fn new_in(allocator: A) -> Self {
        PoolAlloc(Rc::new(PoolAllocInner {
            allocator: RwLock::new(allocator),
            buckets: RwLock::new(BTreeMap::new())
        }))
    }

    // fn get_free_bucket<'b>(buckets: RwLockReadGuard<'b, BTreeMap<usize, Bucket<A>>>, size: usize) -> Option<&'b Bucket<A>> {
        
    // }

    pub fn alloc<'d, R: AsRef<[u8]>>(&'d self, slice: R) -> Result<Ref<'d, A>, AllocErr> {
        let buckets = self.buckets.read().unwrap();
        let slice = slice.as_ref();
        let size = slice.len();

        // let bucket = PoolAlloc::get_free_bucket(self.buckets.read().unwrap(), size);
        {
            let mut outer_bucket = None;
            
            for (&bucket_size, bucket) in buckets.range(size..size*2+1) {
                // We can't do anything if the bucket is full, since there's no memory to hand out,
                // but there might be space in another bucket!
                if !bucket.is_full() {
                    // We now have a free slab that we know we can reuse
                    outer_bucket = Some(bucket);
                }
            }
            
            // Try to get a vacant bucket. If we have one, use it.
            if let Some(bucket) = outer_bucket {
                // The previous line verified that we have at least one empty entry, so we can unwrap
                let reference = bucket.get_and_mark(self.0.clone()).unwrap();

                // TODO: Could this hurt us if we ever have generics here?
                // it might copy T where T is not a copy type...

                // Copy the contents of the slice into our allocator
                unsafe { ptr::copy(slice.as_ptr(), reference.data.as_mut_ptr(), size); }

                return Ok(reference);
            }
        }
        
        let allocated = unsafe { alloc_slice(size, &mut *self.allocator.write().unwrap())? };

        // Darn, didn't get a cached allocation. We now have to insert.
        // We either have no key, in which case we need to insert into the map,
        // or we have a key, so we need to push onto the bucket.
        if buckets.contains_key(&size) {
            // Need to push
            buckets.get(&size).unwrap().push_entry(allocated);

            // Transmute is safe because we keep track of which pointers are already
            // in use.
            Ok(Ref::from(&self.0, unsafe { mem::transmute::<_, &'d mut [u8]>(allocated) }))
        } else {
            // Need to insert
            // Drop the read lock (so we don't deadlock) and get a write lock.
            // We won't end up in an inconsistent state because all the previous operations
            // are either reads or short-circuits.
            drop(buckets);

            let mut buckets = self.buckets.write().unwrap();
            let bucket = Bucket::new(self.0.clone());
            bucket.push_entry(allocated);
            buckets.insert(size, bucket);

            // Transmute is safe because we keep track of which pointers are already
            // in use.
            Ok(Ref::from(&self.0, unsafe { mem::transmute::<_, &'d mut [u8]>(allocated) }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::heap::Heap;
    #[test]
    fn does_not_completely_break() {
        let a = PoolAlloc::new_in(Heap);

        let foo = a.alloc("ten bytes!").expect("Allocation failure!");
        let bar = a.alloc("ten bytes!").expect("Allocation failure!");

        drop(foo);

        println!("{:?}", &*bar);
    }
}


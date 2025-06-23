
pub struct FixedSyncHashStore<T, H> {
    len: usize,
    hasher: H,
}
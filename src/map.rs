use crate::Cache;


pub struct MapCache<C>(C);

impl<K, V, C: Cache<Value = (K, V)>> MapCache<C> {
    
}
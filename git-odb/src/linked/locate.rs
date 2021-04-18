use crate::{compound, linked, pack};

impl linked::Db {
    /// Find an object as identified by [`ObjectId`][git_hash::ObjectId] and store its data in full in the provided `buffer`.
    /// This will search the object in all contained compound object databases.
    /// Use a `pack_cache` to accelerate pack access by reducing the amount of work duplication, or [`pack::cache::Noop`] to disable any caching.
    pub fn locate<'a>(
        &self,
        id: impl AsRef<git_hash::oid>,
        buffer: &'a mut Vec<u8>,
        pack_cache: &mut impl pack::cache::DecodeEntry,
    ) -> Result<Option<compound::Object<'a>>, compound::locate::Error> {
        use compound::locate::LooseOrPack;
        let id = id.as_ref();
        for db in self.dbs.iter() {
            match db.internal_locate(id)? {
                Some(LooseOrPack::Loose(object)) => return Ok(Some(compound::Object::Loose(object))),
                Some(LooseOrPack::Packed(pack_index, object_index)) => {
                    return db
                        .internal_get_packed_object_by_index(pack_index, object_index, buffer, pack_cache)
                        .map(|object| Some(compound::Object::Borrowed(object)))
                        .map_err(Into::into)
                }
                None => continue,
            }
        }
        Ok(None)
    }
}
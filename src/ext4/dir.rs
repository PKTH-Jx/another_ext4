use super::Ext4;
use crate::constants::*;
use crate::ext4_defs::*;
use crate::prelude::*;

impl Ext4 {
    /// Create a new directory. `path` is the absolute path of the new directory.
    pub fn make_dir(&mut self, path: &str) -> Result<()> {
        self.generic_open(
            EXT4_ROOT_INO,
            path,
            OpenFlags::O_CREAT,
            Some(FileType::Directory),
        )
        .map(|_| {
            info!("ext4_dir_mk: {} ok", path);
        })
    }

    /// Remove a directory recursively. `path` is the absolute path of the directory.
    pub fn remove_dir(&mut self, path: &str) -> Result<()> {
        let mut dir = self.generic_open(
            EXT4_ROOT_INO,
            path,
            OpenFlags::O_RDONLY,
            Some(FileType::Directory),
        )?;
        self.remove_dir_recursive(&mut dir)
    }

    /// Find a directory entry that matches a given name under a parent directory
    pub(super) fn dir_find_entry(&self, dir: &InodeRef, name: &str) -> Result<DirEntry> {
        info!("Dir find entry: dir {}, name {}", dir.id, name);
        let inode_size: u32 = dir.inode.size;
        let total_blocks: u32 = inode_size / BLOCK_SIZE as u32;

        let mut iblock: LBlockId = 0;
        while iblock < total_blocks {
            // Get the fs block id
            let fblock = self.extent_get_pblock(dir, iblock)?;
            // Load block from disk
            let block = self.read_block(fblock);
            // Find the entry in block
            let res = Self::find_entry_in_block(&block, name);
            if let Ok(r) = res {
                return Ok(r);
            }
            iblock += 1;
        }
        Err(Ext4Error::new(ErrCode::ENOENT))
    }

    /// Add an entry to a directory
    pub(super) fn dir_add_entry(
        &mut self,
        dir: &mut InodeRef,
        child: &InodeRef,
        name: &str,
    ) -> Result<()> {
        info!(
            "Dir add entry: dir {}, child {}, name {}",
            dir.id, child.id, name
        );
        let inode_size = dir.inode.size();
        let total_blocks = inode_size as u32 / BLOCK_SIZE as u32;

        // Try finding a block with enough space
        let mut iblock: LBlockId = 0;
        while iblock < total_blocks {
            // Get the parent physical block id
            let fblock = self.extent_get_pblock(dir, iblock)?;
            // Load the parent block from disk
            let mut block = self.read_block(fblock);
            // Try inserting the entry to parent block
            if self.insert_entry_to_old_block(&mut block, child, name) {
                return Ok(());
            }
            // Current block has no enough space
            iblock += 1;
        }

        // No free block found - needed to allocate a new data block
        // Append a new data block
        let (_, fblock) = self.inode_append_block(dir)?;
        // Load new block
        let mut new_block = self.read_block(fblock);
        // Write the entry to block
        self.insert_entry_to_new_block(&mut new_block, child, name);

        Ok(())
    }

    /// Remove a entry from a directory
    pub(super) fn dir_remove_entry(&mut self, dir: &mut InodeRef, name: &str) -> Result<()> {
        info!("Dir remove entry: dir {}, path {}", dir.id, name);
        let inode_size = dir.inode.size();
        let total_blocks = inode_size as u32 / BLOCK_SIZE as u32;

        // Check each block
        let mut iblock: LBlockId = 0;
        while iblock < total_blocks {
            // Get the parent physical block id
            let fblock = self.extent_get_pblock(dir, iblock)?;
            // Load the block from disk
            let mut block = self.read_block(fblock);
            // Try removing the entry
            if let Ok(()) = Self::remove_entry_from_block(&mut block, name) {
                self.write_block(&block);
                return Ok(());
            }
            // Current block has no enough space
            iblock += 1;
        }

        // Not found the target entry
        Err(Ext4Error::new(ErrCode::ENOENT))
    }

    /// Get all entries under a directory
    pub(super) fn dir_get_all_entries(&self, dir: &mut InodeRef) -> Result<Vec<DirEntry>> {
        info!("Dir get all entries: dir {}", dir.id);
        let inode_size: u32 = dir.inode.size;
        let total_blocks: u32 = inode_size / BLOCK_SIZE as u32;
        let mut entries: Vec<DirEntry> = Vec::new();

        let mut iblock: LBlockId = 0;
        while iblock < total_blocks {
            // Get the fs block id
            let fblock = self.extent_get_pblock(dir, iblock)?;
            // Load block from disk
            let block = self.read_block(fblock);
            // Get all entries from block
            Self::get_all_entries_from_block(&block, &mut entries);
            iblock += 1;
        }
        Ok(entries)
    }

    /// Find a directory entry that matches a given name in a given block
    fn find_entry_in_block(block: &Block, name: &str) -> Result<DirEntry> {
        info!("Dir find entry {} in block {}", name, block.block_id);
        let mut offset = 0;
        while offset < BLOCK_SIZE {
            let de: DirEntry = block.read_offset_as(offset);
            debug!("Dir entry: {} {:?}", de.rec_len(), de.name());
            if !de.unused() && de.compare_name(name) {
                return Ok(de);
            }
            offset += de.rec_len() as usize;
        }
        Err(Ext4Error::new(ErrCode::ENOENT))
    }

    /// Remove a directory entry that matches a given name from a given block
    fn remove_entry_from_block(block: &mut Block, name: &str) -> Result<()> {
        info!("Dir remove entry {} from block {}", name, block.block_id);
        let mut offset = 0;
        while offset < BLOCK_SIZE {
            let mut de: DirEntry = block.read_offset_as(offset);
            if !de.unused() && de.compare_name(name) {
                // Mark the target entry as unused
                de.set_unused();
                block.write_offset_as(offset, &de);
                return Ok(());
            }
            offset += de.rec_len() as usize;
        }
        Err(Ext4Error::new(ErrCode::ENOENT))
    }

    /// Get all directory entries from a given block
    fn get_all_entries_from_block(block: &Block, entries: &mut Vec<DirEntry>) {
        info!("Dir get all entries from block {}", block.block_id);
        let mut offset = 0;
        while offset < BLOCK_SIZE {
            let de: DirEntry = block.read_offset_as(offset);
            offset += de.rec_len() as usize;
            if !de.unused() {
                debug!("Dir entry: {} {:?}", de.rec_len(), de.name());
                entries.push(de);
            }
        }
    }

    /// Insert a directory entry of a child inode into a new parent block.
    /// A new block must have enough space
    fn insert_entry_to_new_block(&self, dst_blk: &mut Block, child: &InodeRef, name: &str) {
        // Set the entry
        let rec_len = BLOCK_SIZE - size_of::<DirEntryTail>();
        let new_entry = DirEntry::new(
            child.id,
            rec_len as u16,
            name,
            inode_mode2file_type(child.inode.mode()),
        );
        // Write entry to block
        dst_blk.write_offset_as(0, &new_entry);

        // Set tail
        let mut tail = DirEntryTail::default();
        tail.rec_len = size_of::<DirEntryTail>() as u16;
        tail.reserved_ft = 0xDE;
        tail.set_csum(&self.super_block, &new_entry, &dst_blk.data[..]);
        // Copy tail to block
        let tail_offset = BLOCK_SIZE - size_of::<DirEntryTail>();
        dst_blk.write_offset_as(tail_offset, &tail);

        // Sync block to disk
        self.write_block(&dst_blk);
    }

    /// Try insert a directory entry of child inode into a parent block.
    /// Return true if the entry is successfully inserted.
    fn insert_entry_to_old_block(&self, dst_blk: &mut Block, child: &InodeRef, name: &str) -> bool {
        let required_size = DirEntry::required_size(name.len());
        let mut offset = 0;

        while offset < dst_blk.data.len() {
            let mut de: DirEntry = dst_blk.read_offset_as(offset);
            let rec_len = de.rec_len() as usize;

            // Try splitting dir entry
            // The size that `de` actually uses
            let used_size = de.used_size();
            // The rest size
            let free_size = rec_len - used_size;

            // Compare size
            if free_size < required_size {
                // No enough space, try next dir ent
                offset = offset + rec_len;
                continue;
            }
            // Has enough space
            // Update the old entry
            de.set_rec_len(used_size as u16);
            dst_blk.write_offset_as(offset, &de);
            // Insert the new entry
            let new_entry = DirEntry::new(
                child.id,
                free_size as u16,
                name,
                inode_mode2file_type(child.inode.mode()),
            );
            dst_blk.write_offset_as(offset + used_size, &new_entry);

            // Set tail csum
            let tail_offset = BLOCK_SIZE - size_of::<DirEntryTail>();
            let mut tail = dst_blk.read_offset_as::<DirEntryTail>(tail_offset);
            tail.set_csum(&self.super_block, &de, &dst_blk.data[offset..]);
            // Write tail to blk_data
            dst_blk.write_offset_as(tail_offset, &tail);

            // Sync to disk
            self.write_block(&dst_blk);
            return true;
        }
        false
    }

    /// Remove a directory recursively.
    fn remove_dir_recursive(&mut self, dir: &mut InodeRef) -> Result<()> {
        let entries = self.dir_get_all_entries(dir)?;
        for entry in entries {
            if entry.compare_name(".") || entry.compare_name("..") {
                continue;
            }
            let mut child = self.read_inode(entry.inode());
            if child.inode.is_dir(&self.super_block) {
                // Remove child dir recursively
                self.remove_dir_recursive(&mut child)?;
            } else {
                // Remove file
                self.generic_remove(
                    dir.id,
                    &entry.name().map_err(|_| {
                        Ext4Error::with_message(ErrCode::EINVAL, "Invalid dir entry name")
                    })?,
                    Some(FileType::RegularFile),
                )?;
            }
        }
        Ok(())
    }
}

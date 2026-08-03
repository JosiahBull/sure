//! Ceilings for reading a zip that arrived from an untrusted upload.
//!
//! Three importers take a zip — myIR spreadsheets ([`crate::myir`]), a Sharesies export
//! ([`crate::sharesies`]), ASB transaction CSVs ([`crate::asb`]) — and each needs the same
//! guarantee. The HTTP body limit bounds what *arrives*; nothing about it bounds what the
//! archive expands **to**, and that gap is the whole of a zip bomb: a hundred kilobytes on
//! the wire can declare gigabytes of content, and a plain `read_to_end` will faithfully try
//! to allocate them.
//!
//! It lives here rather than in each parser so a fourth importer inherits the ceilings
//! instead of rediscovering the need for them.

use std::io::Read;

/// Entries read from one upload. Enough for a whole loan's worth of statements or every
/// account at a bank; far below anything that threatens the process.
pub const ENTRIES: usize = 64;
/// Uncompressed bytes allowed for any single entry, and across the whole upload.
pub const ENTRY_BYTES: u64 = 16 * 1024 * 1024;
pub const TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// A running budget over one upload's entries. Built once per upload, spent per entry.
#[derive(Debug)]
pub struct Budget {
    entry_bytes: u64,
    total_bytes: u64,
    spent: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Self::new(ENTRY_BYTES, TOTAL_BYTES)
    }
}

impl Budget {
    pub fn new(entry_bytes: u64, total_bytes: u64) -> Self {
        Self {
            entry_bytes,
            total_bytes,
            spent: 0,
        }
    }

    /// Read one entry into memory, refusing it on the size it *declares* and again on the
    /// bytes it actually delivers.
    ///
    /// Two independent bounds, because each covers the other's blind spot: the declared size
    /// is free to check and stops an honest bomb before a byte is decompressed, but a crafted
    /// archive is free to under-declare — so the read is capped as well, at whichever is
    /// tighter, this entry's ceiling or what's left of the upload's. Both the declared and
    /// the delivered size are charged to the budget, so under-declaring buys nothing.
    pub fn read(
        &mut self,
        name: &str,
        declared: u64,
        reader: &mut impl Read,
    ) -> anyhow::Result<Vec<u8>> {
        if declared > self.entry_bytes {
            anyhow::bail!("{name} expands to {declared} bytes, over the limit");
        }
        let room = self.total_bytes.saturating_sub(self.spent);
        if declared > room || room == 0 {
            anyhow::bail!("the upload expands to more than {} bytes", self.total_bytes);
        }
        let cap = self.entry_bytes.min(room);
        let mut buf = Vec::new();
        let read = reader.take(cap + 1).read_to_end(&mut buf)? as u64;
        if read > cap {
            // Name whichever ceiling actually bound: blaming the per-entry limit for an
            // upload that simply ran out of room sends the reader looking in the wrong place.
            if cap < self.entry_bytes {
                anyhow::bail!("the upload expands to more than {} bytes", self.total_bytes);
            }
            anyhow::bail!("{name} expands past the {} byte limit", self.entry_bytes);
        }
        self.spent = self.spent.saturating_add(read.max(declared));
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_declaring_more_than_its_ceiling_is_refused_before_reading() {
        let mut budget = Budget::new(10, 100);
        // The reader is never touched: an empty one still produces the declared-size error.
        let err = budget
            .read("big", 11, &mut std::io::empty())
            .expect_err("refused")
            .to_string();
        assert!(err.contains("over the limit"), "{err:?}");
    }

    /// The blind spot the second bound covers: a crafted archive may declare one byte and
    /// deliver megabytes.
    #[test]
    fn an_entry_that_under_declares_is_still_capped() {
        let mut budget = Budget::new(8, 100);
        let err = budget
            .read("liar", 1, &mut [b'x'; 64].as_slice())
            .expect_err("refused")
            .to_string();
        assert!(err.contains("expands past"), "{err:?}");
    }

    #[test]
    fn entries_share_one_upload_wide_budget() {
        let mut budget = Budget::new(10, 20);
        assert_eq!(
            budget
                .read("a", 10, &mut [b'x'; 10].as_slice())
                .unwrap()
                .len(),
            10
        );
        assert_eq!(
            budget
                .read("b", 10, &mut [b'x'; 10].as_slice())
                .unwrap()
                .len(),
            10
        );
        let err = budget
            .read("c", 1, &mut [b'x'].as_slice())
            .expect_err("refused")
            .to_string();
        assert!(err.contains("more than 20 bytes"), "{err:?}");
    }

    /// Under-declaring must not buy budget either: what actually arrived is charged.
    #[test]
    fn the_budget_is_charged_what_arrived_not_what_was_claimed() {
        let mut budget = Budget::new(10, 12);
        budget.read("a", 0, &mut [b'x'; 10].as_slice()).unwrap();
        let err = budget
            .read("b", 0, &mut [b'x'; 10].as_slice())
            .expect_err("refused")
            .to_string();
        assert!(err.contains("more than 12 bytes"), "{err:?}");
    }

    #[test]
    fn an_honest_entry_reads_through() {
        let mut budget = Budget::default();
        let body = b"Date,Amount\n2020/01/01,-1.00\n";
        assert_eq!(
            budget
                .read("ok.csv", body.len() as u64, &mut body.as_slice())
                .unwrap(),
            body
        );
    }
}

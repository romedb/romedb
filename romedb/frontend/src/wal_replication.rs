//! Read-only WAL bookkeeping for a serialized, single-owner cloud runtime.
use crate::{Connection, Error, Result, TransactionState};

/// End of the committed WAL prefix observed by this connection.
/// Callers must hold exclusive application ownership while sampling and copying.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalPosition {
    pub checkpoint_sequence: u32,
    pub page_size: u32,
    pub committed_frames: u64,
}

impl WalPosition {
    /// Byte length including the 32-byte WAL header, or zero for an empty WAL.
    pub fn byte_len(self) -> Option<u64> {
        if self.committed_frames == 0 {
            return Some(0);
        }
        self.committed_frames
            .checked_mul(u64::from(self.page_size) + 24)?
            .checked_add(32)
    }
}

impl Connection {
    /// Sample a committed WAL boundary, never an active transaction's tail.
    ///
    /// Only available for `Database::open_with_manual_wal`. Other connections,
    /// explicit checkpoints, and external file access must be serialized by the
    /// owner. This is a local observation, not proof of remote publication.
    pub fn wal_replication_position(&self) -> Result<WalPosition> {
        if !self.manual_wal {
            return Err(Error::Validation(
                "WAL replication requires manual WAL mode".into(),
            ));
        }
        if self.transaction_state() != TransactionState::Autocommit {
            return Err(Error::Validation(
                "cannot capture WAL during an active transaction".into(),
            ));
        }
        let state = self.engine.wal_state()?;
        let page_size = self.engine.get_page_size().get();
        if state.max_frame > 0 {
            let mut frame = vec![0; page_size as usize + 24];
            let info = self.engine.wal_get_frame(state.max_frame, &mut frame)?;
            if !info.is_commit_frame() {
                return Err(Error::Validation(
                    "WAL boundary is not a commit frame".into(),
                ));
            }
        }
        Ok(WalPosition {
            checkpoint_sequence: state.checkpoint_seq_no,
            page_size,
            committed_frames: state.max_frame,
        })
    }
}

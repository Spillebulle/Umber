//! Undo/redo.
//!
//! Rather than snapshotting the whole layer per stroke (16 MB at 2048², which
//! would blow past a gigabyte in ~60 strokes), we store only the rectangle a
//! stroke actually touched. A typical stroke damages a small fraction of the
//! canvas, so this keeps a deep history in a modest budget.

use crate::geom::PixelRect;

/// The RGBA8 contents of a rectangle, tightly packed at `width * 4` per row.
#[derive(Clone, Debug)]
pub struct PixelPatch {
    pub rect: PixelRect,
    pub bytes: Vec<u8>,
}

impl PixelPatch {
    pub fn new(rect: PixelRect, bytes: Vec<u8>) -> Self {
        debug_assert_eq!(
            bytes.len() as u64,
            rect.area() * 4,
            "patch byte count must match rect area"
        );
        Self { rect, bytes }
    }

    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }
}

/// A bounded two-stack undo history.
#[derive(Debug)]
pub struct History {
    undo: Vec<PixelPatch>,
    redo: Vec<PixelPatch>,
    used_bytes: usize,
    budget_bytes: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::with_budget(512 * 1024 * 1024)
    }
}

impl History {
    pub fn with_budget(budget_bytes: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            used_bytes: 0,
            budget_bytes,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    /// Record the pre-stroke contents of the damaged region. Invalidates redo,
    /// as any new edit does.
    pub fn record(&mut self, patch: PixelPatch) {
        self.used_bytes += patch.byte_len();
        for p in self.redo.drain(..) {
            self.used_bytes -= p.byte_len();
        }
        self.undo.push(patch);
        self.evict_to_budget();
    }

    /// Pop the state to restore. The caller must capture the *current* contents
    /// of the same rect first and hand it to [`History::push_redo`].
    pub fn take_undo(&mut self) -> Option<PixelPatch> {
        let p = self.undo.pop()?;
        self.used_bytes -= p.byte_len();
        Some(p)
    }

    pub fn take_redo(&mut self) -> Option<PixelPatch> {
        let p = self.redo.pop()?;
        self.used_bytes -= p.byte_len();
        Some(p)
    }

    pub fn push_redo(&mut self, patch: PixelPatch) {
        self.used_bytes += patch.byte_len();
        self.redo.push(patch);
    }

    /// Re-push onto the undo stack without discarding redo — used when redoing.
    pub fn push_undo(&mut self, patch: PixelPatch) {
        self.used_bytes += patch.byte_len();
        self.undo.push(patch);
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.used_bytes = 0;
    }

    /// Drop the oldest undo entries until we fit. The most recent history is
    /// what users actually reach for, so age out from the bottom.
    fn evict_to_budget(&mut self) {
        while self.used_bytes > self.budget_bytes && self.undo.len() > 1 {
            let p = self.undo.remove(0);
            self.used_bytes -= p.byte_len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(w: u32, h: u32, fill: u8) -> PixelPatch {
        let rect = PixelRect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        PixelPatch::new(rect, vec![fill; (w * h * 4) as usize])
    }

    #[test]
    fn undo_redo_round_trips() {
        let mut h = History::default();
        h.record(patch(4, 4, 1));
        assert!(h.can_undo());

        let undone = h.take_undo().unwrap();
        h.push_redo(patch(4, 4, 2));
        assert_eq!(undone.bytes[0], 1);
        assert!(h.can_redo());

        let redone = h.take_redo().unwrap();
        assert_eq!(redone.bytes[0], 2);
    }

    #[test]
    fn recording_clears_redo() {
        let mut h = History::default();
        h.record(patch(4, 4, 1));
        h.take_undo();
        h.push_redo(patch(4, 4, 2));
        assert!(h.can_redo());

        h.record(patch(4, 4, 3));
        assert!(!h.can_redo(), "a new edit must invalidate redo");
    }

    #[test]
    fn budget_evicts_oldest_first() {
        // Budget fits about two 16x16 patches (1024 bytes each).
        let mut h = History::with_budget(2500);
        for i in 0..8u8 {
            h.record(patch(16, 16, i));
        }
        assert!(h.used_bytes() <= 2500, "used {}", h.used_bytes());
        // The newest entry must survive eviction.
        assert_eq!(h.take_undo().unwrap().bytes[0], 7);
    }

    #[test]
    fn accounting_stays_balanced() {
        let mut h = History::default();
        h.record(patch(8, 8, 1));
        h.record(patch(8, 8, 2));
        while h.take_undo().is_some() {}
        assert_eq!(h.used_bytes(), 0);
    }
}

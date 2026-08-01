//! Several documents open at once.
//!
//! The design draws a strip of document tabs above the tool options, and this
//! is the model behind it. It answers one question: what belongs to a document
//! and what belongs to the application?
//!
//! **Per document** — the [`Document`] itself, its [`LayerStack`], its
//! [`History`] and its [`Camera`], together with the tab metadata here. Those
//! four are exactly the things that would be wrong to share: two documents that
//! shared an undo stack would replay each other's strokes, and two that shared
//! a camera would jump when you switched between them.
//!
//! **Global** — everything else. Theme, accent, tool, brush and presets,
//! colour, pressure model and, emphatically, the panel layout: a painter who
//! arranges their workspace does not expect it to rearrange itself when they
//! switch tab. That is the difference between tabs that feel like one
//! application and tabs that feel like several.
//!
//! # Where the live document lives
//!
//! The state of the *active* document is not held here. It stays in the
//! [`Editor`](crate::editor::Editor)'s own fields, where the whole interface
//! already reads it. Only the documents in the background are parked in their
//! tabs, and switching is a swap of a handful of `Vec`s — no pixels move.
//! [`Tab::parked`] is therefore `None` for exactly one tab, the active one.

use std::path::PathBuf;

use glam::UVec2;
use umber_core::{Camera, Document, History, LayerStack};

/// Identity of an open document, stable for as long as it stays open.
///
/// The GPU side keys its per-document textures on this rather than on a tab
/// position, so closing a tab cannot leave the document that shuffles into its
/// place drawing into the dead one's layer array.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocId(u64);

/// The engine state belonging to one document.
///
/// Moving one of these in and out of the editor is what a tab switch is. Every
/// field is either `Copy` or a handful of `Vec` headers, so the swap costs
/// nothing that scales with the size of the artwork.
#[derive(Debug)]
pub struct DocumentState {
    pub doc: Document,
    pub layers: LayerStack,
    pub history: History,
    pub camera: Camera,
}

impl DocumentState {
    /// A blank document of the given size, framed at 100%.
    ///
    /// The camera is corrected to fit as soon as the canvas region is known —
    /// see [`Editor::fit_view`](crate::editor::Editor::fit_view).
    pub fn blank(doc: Document) -> Self {
        Self {
            camera: Camera {
                center: doc.size_vec2() * 0.5,
                zoom: 1.0,
            },
            doc,
            layers: LayerStack::new(),
            history: History::default(),
        }
    }
}

/// One entry in the tab strip.
#[derive(Debug)]
pub struct Tab {
    pub id: DocId,
    pub title: String,
    /// The file this document was opened from or last saved to.
    ///
    /// `None` for a document that has never been written, which is what makes
    /// Save ask for a file the first time and not the second.
    pub path: Option<PathBuf>,
    /// True once anything has changed the pixels since the last save.
    ///
    /// Set by [`Session::mark_modified`] and cleared only by
    /// [`Session::mark_saved`]. What it really tracks is whether closing the
    /// tab would *lose* anything, which is the question the close prompt asks.
    pub modified: bool,
    /// What the import could not represent, already phrased for the user by
    /// `umber_core::docimport`. Kept on the tab so the notice can be reopened
    /// after it has been dismissed.
    pub notes: Vec<String>,
    /// This document's state while another document is being edited. `None` for
    /// the active tab, whose state is live in the editor — see the module docs.
    parked: Option<DocumentState>,
}

impl Tab {
    /// Canvas size, for the tab's tooltip. `None` for the active tab, whose
    /// document the caller already has.
    pub fn parked_size(&self) -> Option<UVec2> {
        self.parked.as_ref().map(|s| s.doc.size)
    }

    /// Everything needed to rebuild this document's GPU storage: canvas size
    /// and how many texture-array slices its layers occupy. `None` for the
    /// active tab, as [`Tab::parked_size`].
    pub fn parked_storage(&self) -> Option<(UVec2, u32)> {
        self.parked
            .as_ref()
            .map(|s| (s.doc.size, s.layers.slot_capacity_needed()))
    }
}

/// Every open document, and which of them is being edited.
#[derive(Debug)]
pub struct Session {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    untitled: u32,
}

impl Default for Session {
    fn default() -> Self {
        let mut session = Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 0,
            untitled: 0,
        };
        let id = session.mint_id();
        let title = session.next_untitled_title();
        session.tabs.push(Tab {
            id,
            title,
            path: None,
            modified: false,
            notes: Vec::new(),
            parked: None,
        });
        session
    }
}

impl Session {
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active_tab(&self) -> &Tab {
        // `active` is maintained by `set_active` and `remove`, both of which
        // clamp; a session always has at least one tab.
        &self.tabs[self.active]
    }

    pub fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    pub fn active_id(&self) -> DocId {
        self.active_tab().id
    }

    pub fn active_title(&self) -> &str {
        &self.active_tab().title
    }

    /// Note that the live document has been changed.
    pub fn mark_modified(&mut self) {
        self.active_tab_mut().modified = true;
    }

    /// Note that the live document now exists on disk at `path`.
    ///
    /// The tab takes the file's own name, so a Save as… is visible in the strip
    /// rather than leaving the tab claiming to be the document it was copied
    /// from.
    pub fn mark_saved(&mut self, path: PathBuf) {
        let tab = self.active_tab_mut();
        if let Some(name) = path.file_name() {
            tab.title = name.to_string_lossy().into_owned();
        }
        tab.path = Some(path);
        tab.modified = false;
    }

    /// Add a tab for a document whose state the caller is about to make live.
    ///
    /// `live` is the state of the document being left behind; it is parked in
    /// the tab it belongs to. The new tab becomes active with no parked state
    /// of its own, which is the invariant the module docs describe.
    pub fn open(&mut self, title: String, path: Option<PathBuf>, live: DocumentState) -> DocId {
        self.park_active(live);
        let id = self.mint_id();
        self.tabs.push(Tab {
            id,
            title,
            path,
            modified: false,
            notes: Vec::new(),
            parked: None,
        });
        self.active = self.tabs.len() - 1;
        id
    }

    /// Park the live state in the tab it came from.
    pub fn park_active(&mut self, state: DocumentState) {
        let tab = &mut self.tabs[self.active];
        debug_assert!(
            tab.parked.is_none(),
            "the active tab must not already hold parked state",
        );
        tab.parked = Some(state);
    }

    /// Take a background tab's state, to be installed as the live document.
    pub fn take_parked(&mut self, index: usize) -> Option<DocumentState> {
        self.tabs.get_mut(index)?.parked.take()
    }

    pub fn set_active(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
        }
    }

    /// Which tab takes over when `index` is closed.
    ///
    /// In the numbering *before* the removal: the tab to the right, or the one
    /// to the left when closing the rightmost. `None` when `index` is the only
    /// tab, which is why the last document's tab shows no close mark.
    pub fn successor_of(&self, index: usize) -> Option<usize> {
        if self.tabs.len() <= 1 || index >= self.tabs.len() {
            return None;
        }
        Some(if index + 1 < self.tabs.len() {
            index + 1
        } else {
            index - 1
        })
    }

    /// Remove a tab, leaving the same document active as before.
    ///
    /// When the closed tab was the active one, the successor from
    /// [`Session::successor_of`] takes over — and the caller must install that
    /// tab's parked state, because the live state it replaces belonged to the
    /// document being closed.
    pub fn remove(&mut self, index: usize) -> Option<Tab> {
        if self.tabs.len() <= 1 || index >= self.tabs.len() {
            return None;
        }
        let closed = self.tabs.remove(index);
        // Positions shift left past the hole, so the *document* that was active
        // has to be tracked rather than the index it used to sit at.
        if index < self.active {
            self.active -= 1;
        } else if index == self.active {
            self.active = self.active.min(self.tabs.len() - 1);
        }
        Some(closed)
    }

    /// A name for a new blank document: "Untitled 1", "Untitled 2", …
    ///
    /// Counts documents created rather than tabs open, so closing Untitled 2
    /// and making another gives Untitled 3 — reusing the name would put two
    /// identically named tabs in one session over a long sitting.
    pub fn next_untitled_title(&mut self) -> String {
        self.untitled += 1;
        format!("Untitled {}", self.untitled)
    }

    fn mint_id(&mut self) -> DocId {
        let id = DocId(self.next_id);
        self.next_id += 1;
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(size: u32) -> DocumentState {
        DocumentState::blank(Document::new(size, size))
    }

    /// The invariant the whole module rests on: exactly one tab — the active
    /// one — has its state live in the editor rather than parked here.
    fn check_invariant(session: &Session) {
        for (i, tab) in session.tabs().iter().enumerate() {
            assert_eq!(
                tab.parked.is_none(),
                i == session.active_index(),
                "tab {i} parked state disagrees with the active tab",
            );
        }
    }

    #[test]
    fn a_new_session_has_one_untitled_document() {
        let session = Session::default();
        assert_eq!(session.len(), 1);
        assert_eq!(session.active_title(), "Untitled 1");
        assert!(!session.active_tab().modified);
        check_invariant(&session);
    }

    #[test]
    fn opening_parks_the_document_being_left() {
        let mut session = Session::default();
        let first = session.active_id();
        let second = session.open("second".into(), None, state(8));

        assert_ne!(first, second, "ids must be unique per open document");
        assert_eq!(session.active_index(), 1);
        assert_eq!(session.active_title(), "second");
        check_invariant(&session);
        assert_eq!(
            session.tabs()[0].parked_size().map(|s| s.x),
            Some(8),
            "the document that was live should now be parked in tab 0",
        );
    }

    #[test]
    fn a_parked_document_reports_the_slots_its_layers_occupy() {
        // The resume path rebuilds every document's GPU storage from this, and
        // a renderer starts with room for only a handful of slices. Reporting
        // the size alone left a deep stack pointing at slices the texture array
        // did not have — which the commit and undo paths would have handed
        // straight to wgpu.
        let mut deep = state(8);
        for _ in 0..6 {
            deep.layers.add().expect("under the layer cap");
        }
        let wanted = deep.layers.slot_capacity_needed();
        assert!(
            wanted > 4,
            "the test needs more layers than a fresh renderer"
        );

        let mut session = Session::default();
        session.open("second".into(), None, deep);

        assert_eq!(
            session.tabs()[0].parked_storage(),
            Some((UVec2::splat(8), wanted)),
        );
        assert_eq!(
            session.active_tab().parked_storage(),
            None,
            "the live document's storage is the caller's own",
        );
    }

    #[test]
    fn switching_moves_state_between_tabs_without_touching_the_rest() {
        let mut session = Session::default();
        session.open("second".into(), None, state(8));

        // Switch back to tab 0, parking the second document.
        let incoming = session.take_parked(0).expect("tab 0 was parked");
        session.park_active(state(16));
        session.set_active(0);
        check_invariant(&session);

        assert_eq!(incoming.doc.size.x, 8, "tab 0's own document came back");
        assert_eq!(session.tabs()[1].parked_size().map(|s| s.x), Some(16));
    }

    #[test]
    fn the_last_tab_cannot_be_closed() {
        let mut session = Session::default();
        assert!(session.successor_of(0).is_none());
        assert!(session.remove(0).is_none());
        assert_eq!(session.len(), 1);
    }

    #[test]
    fn closing_the_active_tab_hands_over_to_its_neighbour() {
        let mut session = Session::default();
        session.open("second".into(), None, state(8));
        session.open("third".into(), None, state(16));
        assert_eq!(session.active_index(), 2);

        // Closing the rightmost falls back to the left.
        assert_eq!(session.successor_of(2), Some(1));
        let incoming = session.take_parked(1).unwrap();
        let closed = session.remove(2).unwrap();
        assert_eq!(closed.title, "third");
        assert_eq!(session.active_index(), 1);
        assert_eq!(session.active_title(), "second");
        assert_eq!(incoming.doc.size.x, 16);
        check_invariant(&session);
    }

    #[test]
    fn closing_a_background_tab_leaves_the_same_document_active() {
        let mut session = Session::default();
        session.open("second".into(), None, state(8));
        session.open("third".into(), None, state(16));

        let active = session.active_id();
        session.remove(0).unwrap();

        assert_eq!(session.len(), 2);
        assert_eq!(session.active_id(), active, "the wrong tab became active");
        assert_eq!(session.active_title(), "third");
        check_invariant(&session);
    }

    #[test]
    fn untitled_names_do_not_repeat_after_a_close() {
        let mut session = Session::default();
        let name = session.next_untitled_title();
        assert_eq!(name, "Untitled 2");
        session.open(name, None, state(8));
        session.remove(1).unwrap();
        assert_eq!(session.next_untitled_title(), "Untitled 3");
    }

    #[test]
    fn saving_clears_the_flag_and_takes_the_file_name() {
        let mut session = Session::default();
        session.mark_modified();
        session.mark_saved(PathBuf::from("/work/studies/hands.ora"));

        let tab = session.active_tab();
        assert!(!tab.modified, "a saved document has nothing left to lose");
        assert_eq!(tab.title, "hands.ora", "the tab kept its old name");
        assert!(tab.path.is_some());
    }

    #[test]
    fn saving_is_per_document() {
        // The flag and the path both belong to the tab, not to the session:
        // saving one document must not mark another as safe to close.
        let mut session = Session::default();
        session.mark_modified();
        session.open("second".into(), None, state(8));
        session.mark_modified();
        session.mark_saved(PathBuf::from("second.ora"));

        assert!(session.tabs()[0].modified, "the first document was cleared");
        assert!(!session.tabs()[1].modified);
    }

    #[test]
    fn modified_is_per_document() {
        let mut session = Session::default();
        session.mark_modified();
        session.open("second".into(), None, state(8));
        assert!(
            !session.active_tab().modified,
            "a new document is unmodified"
        );
        assert!(
            session.tabs()[0].modified,
            "the first document's flag moved"
        );
    }
}

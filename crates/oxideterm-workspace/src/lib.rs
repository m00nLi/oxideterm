use std::fmt;

mod command_palette;

pub use command_palette::{
    CommandPaletteMatch, CommandPaletteMode, command_palette_match, parse_command_palette_query,
};

pub const MAX_PANES_PER_TAB: usize = 4;
pub const MIN_PANE_FRACTION: f32 = 10.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TerminalSessionId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TabId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct PaneId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TabKind {
    LocalTerminal,
    SshTerminal,
    FileManager,
    Launcher,
    Graphics,
    Runtime,
    ConnectionPool,
    ConnectionMonitor,
    Topology,
    NotificationCenter,
    Sftp,
    Ide,
    Forwards,
    SessionManager,
    PluginManager,
    Plugin { plugin_id: String, tab_id: String },
    CloudSync,
    RemoteDesktop,
    Settings,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TabTitleSource {
    Static,
    I18nKey(&'static str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActiveSessionReadiness {
    Ready,
    Connecting,
    Error,
    Disconnected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveSessionNode {
    pub id: String,
    pub title: String,
    pub port: u16,
    pub terminal_ids: Vec<TerminalSessionId>,
    pub readiness: ActiveSessionReadiness,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActiveSessionStatus {
    Idle,
    Connecting,
    Connected,
    Active,
    Error,
}

impl ActiveSessionNode {
    pub fn has_terminals(&self) -> bool {
        !self.terminal_ids.is_empty()
    }

    pub fn status(&self) -> ActiveSessionStatus {
        match self.readiness {
            ActiveSessionReadiness::Connecting => ActiveSessionStatus::Connecting,
            ActiveSessionReadiness::Ready if self.has_terminals() => ActiveSessionStatus::Active,
            ActiveSessionReadiness::Ready => ActiveSessionStatus::Connected,
            ActiveSessionReadiness::Error => ActiveSessionStatus::Error,
            ActiveSessionReadiness::Disconnected => ActiveSessionStatus::Idle,
        }
    }
}

pub fn sort_active_session_nodes(nodes: &mut [ActiveSessionNode]) {
    nodes.sort_by(|left, right| {
        left.title
            .cmp(&right.title)
            .then_with(|| left.id.cmp(&right.id))
    });
}

#[derive(Clone, Debug)]
pub struct Tab {
    pub id: TabId,
    pub kind: TabKind,
    pub title: String,
    pub custom_title: Option<String>,
    pub title_source: TabTitleSource,
    pub root_pane: Option<PaneNode>,
    pub active_pane_id: Option<PaneId>,
}

impl Tab {
    pub fn display_title(&self) -> &str {
        self.custom_title.as_deref().unwrap_or(&self.title)
    }

    pub fn set_custom_title(&mut self, title: Option<String>) {
        // Do NOT mutate `title_source` here. Both `display_title()` and
        // `tab_display_title()` check `custom_title` first, so the `Custom`
        // variant is redundant. Preserving the original source (Static or
        // I18nKey) keeps live re-translation working after a reset.
        match title {
            Some(ref t) => {
                let trimmed = t.trim();
                if !trimmed.is_empty() && trimmed != self.title {
                    self.custom_title = Some(trimmed.to_string());
                } else {
                    self.custom_title = None;
                }
            }
            None => {
                self.custom_title = None;
            }
        }
    }
}

/// A split child keeps its layout share beside the node it sizes.
///
/// Keeping these values together makes it impossible for tree edits to leave
/// parallel child and size vectors with different lengths.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneSplitChild {
    pub node: PaneNode,
    pub size: f32,
}

impl PaneSplitChild {
    pub fn new(node: PaneNode, size: f32) -> Self {
        Self { node, size }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PaneNode {
    Leaf {
        pane_id: PaneId,
        session_id: TerminalSessionId,
    },
    Group {
        id: PaneId,
        direction: SplitDirection,
        children: Vec<PaneSplitChild>,
    },
}

impl PaneNode {
    pub fn leaf(pane_id: PaneId, session_id: TerminalSessionId) -> Self {
        Self::Leaf {
            pane_id,
            session_id,
        }
    }

    pub fn pane_count(&self) -> usize {
        match self {
            Self::Leaf { .. } => 1,
            Self::Group { children, .. } => {
                children.iter().map(|child| child.node.pane_count()).sum()
            }
        }
    }

    pub fn first_pane_id(&self) -> PaneId {
        match self {
            Self::Leaf { pane_id, .. } => *pane_id,
            Self::Group { children, .. } => children[0].node.first_pane_id(),
        }
    }

    pub fn contains_pane(&self, target: PaneId) -> bool {
        match self {
            Self::Leaf { pane_id, .. } => *pane_id == target,
            Self::Group { children, .. } => children
                .iter()
                .any(|child| child.node.contains_pane(target)),
        }
    }

    pub fn pane_id_for_session(&self, target: TerminalSessionId) -> Option<PaneId> {
        match self {
            Self::Leaf {
                pane_id,
                session_id,
            } if *session_id == target => Some(*pane_id),
            Self::Leaf { .. } => None,
            Self::Group { children, .. } => children
                .iter()
                .find_map(|child| child.node.pane_id_for_session(target)),
        }
    }

    pub fn session_id_for_pane(&self, target: PaneId) -> Option<TerminalSessionId> {
        match self {
            Self::Leaf {
                pane_id,
                session_id,
            } if *pane_id == target => Some(*session_id),
            Self::Leaf { .. } => None,
            Self::Group { children, .. } => children
                .iter()
                .find_map(|child| child.node.session_id_for_pane(target)),
        }
    }

    pub fn replace_session(
        &mut self,
        old_session_id: TerminalSessionId,
        new_pane_id: PaneId,
        new_session_id: TerminalSessionId,
    ) -> Option<PaneId> {
        match self {
            Self::Leaf {
                pane_id,
                session_id,
            } if *session_id == old_session_id => {
                let old_pane_id = *pane_id;
                *pane_id = new_pane_id;
                *session_id = new_session_id;
                Some(old_pane_id)
            }
            Self::Leaf { .. } => None,
            Self::Group { children, .. } => children.iter_mut().find_map(|child| {
                child
                    .node
                    .replace_session(old_session_id, new_pane_id, new_session_id)
            }),
        }
    }

    pub fn collect_pane_ids(&self, panes: &mut Vec<PaneId>) {
        match self {
            Self::Leaf { pane_id, .. } => panes.push(*pane_id),
            Self::Group { children, .. } => {
                for child in children {
                    child.node.collect_pane_ids(panes);
                }
            }
        }
    }

    pub fn collect_session_ids(&self, sessions: &mut Vec<TerminalSessionId>) {
        match self {
            Self::Leaf { session_id, .. } => sessions.push(*session_id),
            Self::Group { children, .. } => {
                for child in children {
                    child.node.collect_session_ids(sessions);
                }
            }
        }
    }

    pub fn split_active(
        &mut self,
        active_pane_id: PaneId,
        group_id: PaneId,
        direction: SplitDirection,
        new_pane_id: PaneId,
        new_session_id: TerminalSessionId,
    ) -> bool {
        match self {
            Self::Leaf {
                pane_id,
                session_id,
            } if *pane_id == active_pane_id => {
                let old = Self::Leaf {
                    pane_id: *pane_id,
                    session_id: *session_id,
                };
                *self = Self::Group {
                    id: group_id,
                    direction,
                    children: vec![
                        PaneSplitChild::new(old, 50.0),
                        PaneSplitChild::new(Self::leaf(new_pane_id, new_session_id), 50.0),
                    ],
                };
                true
            }
            Self::Leaf { .. } => false,
            Self::Group { children, .. } => children.iter_mut().any(|child| {
                child.node.split_active(
                    active_pane_id,
                    group_id,
                    direction,
                    new_pane_id,
                    new_session_id,
                )
            }),
        }
    }

    pub fn close_pane(&mut self, target: PaneId) -> Option<PaneId> {
        match self {
            Self::Leaf { .. } => None,
            Self::Group { children, .. } => {
                let mut removed = false;
                let mut index = 0;
                while index < children.len() {
                    if matches!(&children[index].node, Self::Leaf { pane_id, .. } if *pane_id == target)
                    {
                        children.remove(index);
                        removed = true;
                        break;
                    }
                    if children[index].node.contains_pane(target) {
                        let fallback = children[index].node.close_pane(target);
                        if let Some(replacement) = children[index].node.single_child_replacement() {
                            children[index].node = replacement;
                        }
                        removed = fallback.is_some();
                        break;
                    }
                    index += 1;
                }

                if !removed {
                    return None;
                }

                normalize_children(children);
                children.first().map(|child| child.node.first_pane_id())
            }
        }
    }

    pub fn single_child_replacement(&mut self) -> Option<PaneNode> {
        match self {
            Self::Group { children, .. } if children.len() == 1 => Some(children.remove(0).node),
            _ => None,
        }
    }

    pub fn update_group_sizes(&mut self, group_id: PaneId, next_sizes: &[f32]) -> bool {
        match self {
            Self::Leaf { .. } => false,
            Self::Group { id, children, .. }
            if *id == group_id && next_sizes.len() == children.len() =>
                {
                    let sizes = balanced_sizes(next_sizes, children.len());
                    for (child, size) in children.iter_mut().zip(sizes) {
                        child.size = size;
                    }
                    true
                }
            Self::Group { children, .. } => children
                .iter_mut()
                .any(|child| child.node.update_group_sizes(group_id, next_sizes)),
        }
    }

    pub fn reset_group_sizes(&mut self, group_id: PaneId) -> bool {
        match self {
            Self::Leaf { .. } => false,
            Self::Group { id, children, .. } if *id == group_id => {
                // Reset only the addressed split group so nested pane ratios
                // remain untouched when a sibling divider is double-clicked.
                let sizes = equal_sizes(children.len());
                for (child, size) in children.iter_mut().zip(sizes) {
                    child.size = size;
                }
                true
            }
            Self::Group { children, .. } => children
                .iter_mut()
                .any(|child| child.node.reset_group_sizes(group_id)),
        }
    }

    pub fn split_sizes(&self) -> Vec<f32> {
        match self {
            Self::Group { children, .. } => balanced_sizes(
                &children.iter().map(|child| child.size).collect::<Vec<_>>(),
                children.len(),
            ),
            Self::Leaf { .. } => Vec::new(),
        }
    }
}

pub fn adjusted_split_sizes(
    start_sizes: &[f32],
    handle_index: usize,
    delta_fraction: f32,
) -> Vec<f32> {
    if handle_index + 1 >= start_sizes.len() {
        return start_sizes.to_vec();
    }

    let mut sizes = start_sizes.to_vec();
    let left = start_sizes[handle_index] + delta_fraction;
    let total = start_sizes[handle_index] + start_sizes[handle_index + 1];
    let left = left.clamp(MIN_PANE_FRACTION, total - MIN_PANE_FRACTION);
    sizes[handle_index] = left;
    sizes[handle_index + 1] = total - left;
    sizes
}

pub fn balanced_sizes(sizes: &[f32], count: usize) -> Vec<f32> {
    if count == 0 {
        return Vec::new();
    }
    if sizes.len() != count {
        return equal_sizes(count);
    }
    let total: f32 = sizes.iter().copied().sum();
    if total <= f32::EPSILON {
        return equal_sizes(count);
    }
    sizes.iter().map(|size| (size / total) * 100.0).collect()
}

pub fn equal_sizes(count: usize) -> Vec<f32> {
    vec![100.0 / count as f32; count]
}

fn normalize_children(children: &mut [PaneSplitChild]) {
    let sizes = balanced_sizes(
        &children.iter().map(|child| child.size).collect::<Vec<_>>(),
        children.len(),
    );
    for (child, size) in children.iter_mut().zip(sizes) {
        child.size = size;
    }
}

impl fmt::Display for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tab-{}", self.0)
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pane-{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (PaneId, PaneId, PaneId, TerminalSessionId, TerminalSessionId) {
        (
            PaneId(1),
            PaneId(2),
            PaneId(3),
            TerminalSessionId(1),
            TerminalSessionId(2),
        )
    }

    fn split_children(
        pane_a: PaneId,
        pane_b: PaneId,
        session_a: TerminalSessionId,
        session_b: TerminalSessionId,
        sizes: [f32; 2],
    ) -> Vec<PaneSplitChild> {
        vec![
            PaneSplitChild::new(PaneNode::leaf(pane_a, session_a), sizes[0]),
            PaneSplitChild::new(PaneNode::leaf(pane_b, session_b), sizes[1]),
        ]
    }

    #[test]
    fn split_active_leaf_creates_group_and_focusable_leaf() {
        let (pane_a, pane_b, group, session_a, session_b) = ids();
        let mut node = PaneNode::leaf(pane_a, session_a);

        assert!(node.split_active(pane_a, group, SplitDirection::Horizontal, pane_b, session_b));
        assert_eq!(node.pane_count(), 2);
        assert!(node.contains_pane(pane_a));
        assert!(node.contains_pane(pane_b));
    }

    #[test]
    fn close_pane_collapses_group_to_remaining_leaf() {
        let (pane_a, pane_b, group, session_a, session_b) = ids();
        let mut node = PaneNode::Group {
            id: group,
            direction: SplitDirection::Horizontal,
            children: split_children(pane_a, pane_b, session_a, session_b, [50.0, 50.0]),
        };

        assert_eq!(node.close_pane(pane_b), Some(pane_a));
        if let Some(replacement) = node.single_child_replacement() {
            node = replacement;
        }
        assert_eq!(node, PaneNode::leaf(pane_a, session_a));
    }

    #[test]
    fn locates_pane_by_terminal_session() {
        let (pane_a, pane_b, group, session_a, session_b) = ids();
        let node = PaneNode::Group {
            id: group,
            direction: SplitDirection::Horizontal,
            children: split_children(pane_a, pane_b, session_a, session_b, [50.0, 50.0]),
        };

        assert_eq!(node.pane_id_for_session(session_b), Some(pane_b));
        assert_eq!(node.pane_id_for_session(TerminalSessionId(99)), None);
    }

    #[test]
    fn locates_terminal_session_by_pane() {
        let (pane_a, pane_b, group, session_a, session_b) = ids();
        let node = PaneNode::Group {
            id: group,
            direction: SplitDirection::Horizontal,
            children: split_children(pane_a, pane_b, session_a, session_b, [50.0, 50.0]),
        };

        assert_eq!(node.session_id_for_pane(pane_a), Some(session_a));
        assert_eq!(node.session_id_for_pane(PaneId(99)), None);
    }

    #[test]
    fn collects_terminal_sessions_from_tree() {
        let (pane_a, pane_b, group, session_a, session_b) = ids();
        let node = PaneNode::Group {
            id: group,
            direction: SplitDirection::Horizontal,
            children: split_children(pane_a, pane_b, session_a, session_b, [50.0, 50.0]),
        };
        let mut sessions = Vec::new();

        node.collect_session_ids(&mut sessions);

        assert_eq!(sessions, vec![session_a, session_b]);
    }

    #[test]
    fn replaces_terminal_session_in_place() {
        let (pane_a, pane_b, group, session_a, session_b) = ids();
        let new_pane = PaneId(42);
        let new_session = TerminalSessionId(77);
        let mut node = PaneNode::Group {
            id: group,
            direction: SplitDirection::Horizontal,
            children: split_children(pane_a, pane_b, session_a, session_b, [50.0, 50.0]),
        };

        assert_eq!(
            node.replace_session(session_b, new_pane, new_session),
            Some(pane_b)
        );
        assert_eq!(node.pane_id_for_session(new_session), Some(new_pane));
        assert_eq!(node.pane_id_for_session(session_b), None);
    }

    #[test]
    fn adjusted_split_sizes_clamps_adjacent_panes() {
        assert_eq!(
            adjusted_split_sizes(&[50.0, 50.0], 0, 80.0),
            vec![90.0, 10.0]
        );
        assert_eq!(
            adjusted_split_sizes(&[20.0, 80.0], 0, -50.0),
            vec![10.0, 90.0]
        );
    }

    #[test]
    fn reset_group_sizes_restores_equal_split_for_target_group() {
        let (pane_a, pane_b, group, session_a, session_b) = ids();
        let mut node = PaneNode::Group {
            id: group,
            direction: SplitDirection::Horizontal,
            children: split_children(pane_a, pane_b, session_a, session_b, [70.0, 30.0]),
        };

        assert!(node.reset_group_sizes(group));

        match node {
            PaneNode::Group { children, .. } => {
                assert_eq!(
                    children.iter().map(|child| child.size).collect::<Vec<_>>(),
                    vec![50.0, 50.0]
                );
            }
            PaneNode::Leaf { .. } => panic!("expected split group"),
        }
    }

    fn sample_tab(title: &str) -> Tab {
        Tab {
            id: TabId(1),
            kind: TabKind::LocalTerminal,
            title: title.to_string(),
            custom_title: None,
            title_source: TabTitleSource::Static,
            root_pane: None,
            active_pane_id: None,
        }
    }

    fn sample_i18n_tab(title: &str) -> Tab {
        Tab {
            title_source: TabTitleSource::I18nKey("sidebar.panels.settings"),
            ..sample_tab(title)
        }
    }

    #[test]
    fn display_title_falls_back_to_raw_title_without_custom() {
        let tab = sample_tab("Local");
        assert_eq!(tab.display_title(), "Local");
    }

    #[test]
    fn set_custom_title_stores_trimmed_value() {
        let mut tab = sample_tab("Local");
        tab.set_custom_title(Some("  build  ".to_string()));
        assert_eq!(tab.custom_title.as_deref(), Some("build"));
        // title_source is preserved so locale switches still re-translate.
        assert_eq!(tab.title_source, TabTitleSource::Static);
        assert_eq!(tab.display_title(), "build");
    }

    #[test]
    fn set_custom_title_ignores_blank_and_equal_to_raw() {
        let mut tab = sample_tab("Local");
        // Whitespace-only clears the custom title instead of storing empty.
        tab.set_custom_title(Some("   ".to_string()));
        assert!(tab.custom_title.is_none());
        // A value equal to the raw title is treated as a reset.
        tab.set_custom_title(Some("Local".to_string()));
        assert!(tab.custom_title.is_none());
    }

    #[test]
    fn set_custom_title_none_preserves_original_source() {
        let mut tab = sample_tab("Local");
        tab.set_custom_title(Some("build".to_string()));
        assert_eq!(tab.title_source, TabTitleSource::Static);
        tab.set_custom_title(None);
        assert!(tab.custom_title.is_none());
        assert_eq!(tab.title_source, TabTitleSource::Static);
        assert_eq!(tab.display_title(), "Local");
    }

    #[test]
    fn set_custom_title_preserves_i18n_source_for_retranslation() {
        let mut tab = sample_i18n_tab("Settings");
        tab.set_custom_title(Some("My Settings".to_string()));
        // The i18n source survives so sync_tab_titles can re-translate.
        assert_eq!(tab.title_source, TabTitleSource::I18nKey("sidebar.panels.settings"));
        // Reset back to the derived title.
        tab.set_custom_title(None);
        assert!(tab.custom_title.is_none());
        assert_eq!(tab.title_source, TabTitleSource::I18nKey("sidebar.panels.settings"));
    }
}

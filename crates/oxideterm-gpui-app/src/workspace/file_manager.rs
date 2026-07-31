use std::borrow::Cow;

use super::ime::WorkspaceImeTarget;
use super::*;
use gpui::{AnchoredPositionMode, Corner, UniformListScrollHandle, anchored, deferred, prelude::*};
use oxideterm_gpui_markdown::{
    MarkdownOptions, MarkdownVirtualListScrollHandle, highlight, markdown_virtual_with_code_actions,
};
use oxideterm_gpui_ui::{
    button::{ButtonRadius, ButtonVariant, IconButtonOptions, ToolbarButtonOptions},
    context_menu::{ContextMenuActionableStyle, context_menu_event_boundary},
    modal::{
        dismissible_dialog_backdrop, overlay_content_boundary, quicklook_backdrop,
        rounded_shell_child_radius,
    },
    scroll::ScrollableElement,
    surface::{color_for_background, color_with_background_scaled_alpha},
    text_input::{TextInputView, text_input, text_input_anchor_probe},
};
use oxideterm_local_files::{
    BOOKMARKS_FILENAME as FILE_MANAGER_BOOKMARKS_FILENAME, LocalArchiveEntry, LocalArchiveInfo,
    LocalBookmark, LocalChecksumResult, LocalClipboardMode, LocalDrive, LocalFileEntry,
    LocalFileType, LocalPreview, LocalPreviewMetadata, LocalSidebarLocation,
    LocalSidebarLocationKind, LocalSortDirection, LocalSortField,
};
use oxideterm_preview::{
    AudioPreviewBackend, AudioPreviewCommand, AudioPreviewState, RodioAudioPreviewBackend,
    font_family_name_from_bytes,
};

mod actions;
mod dialogs;
mod helpers;
mod render;

use self::actions::{open_path_external, reveal_path_external};
use self::helpers::*;
use super::sftp::native_video::{SharedSftpNativeVideoSurface, sftp_native_video_element};

const FILE_MANAGER_HEADER_HEIGHT: f32 = 40.0; // Tauri h-10.
const FILE_MANAGER_HEADER_GAP: f32 = 6.0;
const FILE_MANAGER_HEADER_TITLE_MIN_WIDTH: f32 = 32.0;
const FILE_MANAGER_PATH_BAR_HORIZONTAL_PADDING: f32 = 4.0;
const FILE_MANAGER_BREADCRUMB_ROW_GAP: f32 = 1.0;
const FILE_MANAGER_BREADCRUMB_SEGMENT_PADDING: f32 = 3.0;
const FILE_MANAGER_BREADCRUMB_CONTENT_GAP: f32 = 2.0;
const FILE_MANAGER_TOOLBAR_HEIGHT: f32 = 48.0; // Shared top-level tool-page toolbar height.
const FILE_MANAGER_ROW_HEIGHT: f32 = 28.0; // Tauri FileList FILE_ROW_HEIGHT.
const FILE_MANAGER_VIRTUAL_OVERSCAN: usize = 15; // Tauri useVirtualizer overscan.
const FILE_MANAGER_ARCHIVE_LIST_INITIAL_ITEM_COUNT: usize = 0;
const FILE_MANAGER_ARCHIVE_ROW_HEIGHT: f32 = 28.0; // Tauri archive preview row min-h-7.
const FILE_MANAGER_ARCHIVE_LIST_OVERSCAN: usize = 12;
const FILE_MANAGER_PREVIEW_CODE_OVERSCAN: usize = 20; // Tauri VirtualTextPreview OVERSCAN_LINES.
const FILE_MANAGER_PREVIEW_CODE_WRAP_COLUMNS: usize = 96; // Virtual rows pre-wrap long `whitespace-pre` lines.
const FILE_MANAGER_PREVIEW_STREAM_CHUNK_SIZE: u64 = 128 * 1024; // Tauri VirtualTextPreview CHUNK_SIZE.
const FILE_MANAGER_PREVIEW_CODE_GUTTER_ALPHA: u32 = 0x4d; // Tauri CodeHighlight line-number opacity 30%.
const FILE_MANAGER_SIDEBAR_WIDTH: f32 = 184.0; // Compact favorites rail keeps file content visually dominant.
const FILE_MANAGER_SIDEBAR_HIDDEN_WIDTH: f32 = 0.0; // Hidden favorites return all horizontal space to file content.
const FILE_MANAGER_SIDEBAR_ROW_HEIGHT: f32 = 30.0;
const FILE_MANAGER_SIDEBAR_SECTION_HEADER_HEIGHT: f32 = 28.0;
const FILE_MANAGER_SIDEBAR_SECTION_GAP: f32 = 10.0;
const FILE_MANAGER_TEXT_XS: f32 = 12.0;
const FILE_MANAGER_TEXT_SM: f32 = 14.0;
const FILE_MANAGER_ICON_SM: f32 = 12.0;
const FILE_MANAGER_ICON_MD: f32 = 14.0;
const FILE_MANAGER_TOOL_BUTTON: f32 = 24.0;
const FILE_MANAGER_SIZE_COL: f32 = 80.0;
const FILE_MANAGER_MODIFIED_COL: f32 = 96.0;
const FILE_MANAGER_CONTEXT_MENU_WIDTH: f32 = 180.0; // Tauri min-w-[180px].
const FILE_MANAGER_CONTEXT_MENU_MAX_HEIGHT: f32 = 520.0; // Tauri max-h-[80vh], clamped per viewport.
const FILE_MANAGER_CONTEXT_MENU_PADDING: f32 = 4.0;
const FILE_MANAGER_CONTEXT_MENU_ITEM_HEIGHT: f32 = 30.0;
const FILE_MANAGER_DIALOG_WIDTH_SM: f32 = 384.0;
const FILE_MANAGER_QUICKLOOK_WIDTH: f32 = 1000.0; // Tauri QuickLook width: min(90vw, 1000px).
const FILE_MANAGER_QUICKLOOK_HEIGHT: f32 = 800.0; // Tauri QuickLook height: min(90vh, 800px).
const FILE_MANAGER_QUICKLOOK_MIN_WIDTH: f32 = 400.0; // Tauri QuickLook minWidth: min(400px, 95vw).
const FILE_MANAGER_QUICKLOOK_MIN_HEIGHT: f32 = 300.0; // Tauri QuickLook minHeight: min(300px, 95vh).
const FILE_MANAGER_PREVIEW_MIN_ZOOM: f32 = 0.25; // Tauri QuickLook image/PDF minimum zoom.
const FILE_MANAGER_PREVIEW_MAX_ZOOM: f32 = 3.0; // Tauri QuickLook image/PDF maximum zoom.
const FILE_MANAGER_BG_ACTIVE_BG_ALPHA: u32 = 0x66; // [data-bg-active] --color-theme-bg 40%.
const FILE_MANAGER_BG_ACTIVE_PANEL_ALPHA: u32 = 0x66; // [data-bg-active] --color-theme-bg-panel 40%.
const FILE_MANAGER_BG_ACTIVE_HOVER_ALPHA: u32 = 0x80; // [data-bg-active] --color-theme-bg-hover 50%.
const FILE_MANAGER_PANEL_80_ALPHA: u32 = 0xcc; // Tauri bg-theme-bg-panel/80.
const FILE_MANAGER_SELECTED_BG_ALPHA: u32 = 0x33; // Tauri bg-theme-accent/20.
const FILE_MANAGER_BREADCRUMB_ACTIVE_ALPHA: u32 = 0x4d; // Tauri bg-theme-bg-hover/30.
const FILE_MANAGER_BREADCRUMB_HOVER_ALPHA: u32 = 0x80; // Tauri hover:bg-theme-bg-hover/50.
const FILE_MANAGER_DIALOG_BORDER_ALPHA: u32 = 0x99;
const FILE_MANAGER_RED: u32 = 0xf87171; // Tauri text-red-400.
const FILE_MANAGER_BLUE: u32 = 0x60a5fa; // Tauri text-blue-400.
const FILE_MANAGER_GREEN: u32 = 0x22c55e; // Tauri text-green-500.
const FILE_MANAGER_ORANGE: u32 = 0xfb923c; // Tauri text-orange-400.
const FILE_MANAGER_PURPLE: u32 = 0xc084fc; // Tauri preview/file accent family.

#[derive(Clone, Copy, Debug, PartialEq)]
struct FileManagerSidebarItemGeometry {
    transition_index: usize,
    top: f32,
}

impl FileManagerSidebarItemGeometry {
    const FIRST: Self = Self {
        transition_index: 0,
        top: 0.0,
    };

    fn next(self) -> Self {
        // Sidebar rows have a fixed height, so their relative motion remains
        // stable even when the scroll viewport itself moves.
        Self {
            transition_index: self.transition_index + 1,
            top: self.top + FILE_MANAGER_SIDEBAR_ROW_HEIGHT,
        }
    }

    fn after_section_header(self) -> Self {
        // The Locations heading contributes real space between the two row groups.
        Self {
            top: self.top
                + FILE_MANAGER_SIDEBAR_SECTION_GAP
                + FILE_MANAGER_SIDEBAR_SECTION_HEADER_HEIGHT,
            ..self
        }
    }
}

#[cfg(test)]
mod sidebar_geometry_tests {
    use super::*;

    #[test]
    fn sidebar_geometry_preserves_row_and_section_spacing() {
        let second_row = FileManagerSidebarItemGeometry::FIRST.next();
        assert_eq!(second_row.transition_index, 1);
        assert_eq!(second_row.top, FILE_MANAGER_SIDEBAR_ROW_HEIGHT);

        let first_drive = second_row.after_section_header();
        assert_eq!(first_drive.transition_index, 1);
        assert_eq!(
            first_drive.top,
            FILE_MANAGER_SIDEBAR_ROW_HEIGHT
                + FILE_MANAGER_SIDEBAR_SECTION_GAP
                + FILE_MANAGER_SIDEBAR_SECTION_HEADER_HEIGHT
        );
    }
}

fn file_manager_list_virtual_spec() -> TauriVirtualListSpec {
    // Tauri FileList owns FILE_ROW_HEIGHT and useVirtualizer overscan as one
    // contract. Keep native render/scroll call sites on the same named spec so
    // row height and overdraw cannot drift independently.
    TauriVirtualListSpec::new(px(FILE_MANAGER_ROW_HEIGHT), FILE_MANAGER_VIRTUAL_OVERSCAN)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(super) enum FileManagerInput {
    Path,
    Filter,
    DialogValue,
}

impl FileManagerInput {
    pub(super) fn anchor_key(self) -> u64 {
        match self {
            Self::Path => 1,
            Self::Filter => 2,
            Self::DialogValue => 3,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct LocalClipboard {
    mode: LocalClipboardMode,
    paths: Vec<String>,
    source_dir: String,
}

#[derive(Clone, Debug)]
pub(super) struct FileManagerContextMenu {
    file: Option<LocalFileEntry>,
    x: f32,
    y: f32,
}

#[derive(Clone, Debug)]
pub(super) enum FileManagerDialog {
    Drives,
    NewFolder,
    NewFile,
    Rename {
        old_name: String,
    },
    Delete {
        files: Vec<String>,
    },
    EditBookmark {
        id: String,
        path: String,
    },
    Properties {
        entry: LocalFileEntry,
        details: FileManagerProperties,
    },
    Preview {
        entry: LocalFileEntry,
    },
}

#[derive(Clone, Debug)]
pub(super) struct FileManagerProperties {
    kind_label: String,
    location: String,
    size: u64,
    modified: Option<i64>,
    accessed: Option<i64>,
    readonly: bool,
    dir_files: Option<u64>,
    dir_dirs: Option<u64>,
    total_size: Option<u64>,
    created: Option<i64>,
    mode: Option<u32>,
    mime_type: Option<String>,
    is_symlink: bool,
}

#[derive(Clone, Debug)]
pub(super) struct FileManagerOperationProgress {
    pub(super) current: usize,
    pub(super) total: usize,
    pub(super) file_name: String,
    pub(super) active: bool,
}

#[derive(Clone, Debug, Default)]
pub(super) struct FileManagerPreviewStreamState {
    pub(super) path: String,
    pub(super) size: u64,
    pub(super) language: Option<String>,
    pub(super) lines: Vec<String>,
    pub(super) loaded_bytes: u64,
    pub(super) eof: bool,
    pub(super) loading: bool,
    pub(super) error: Option<String>,
    pub(super) carry_text: String,
    pub(super) carry_bytes: Vec<u8>,
}

#[derive(Debug)]
pub(super) enum FileManagerOperationEvent {
    Progress(FileManagerOperationProgress),
    Finished(Result<(), String>),
}

pub(super) struct FileManagerRotatedPreviewImage {
    pub(super) path: String,
    pub(super) rotation: i32,
    pub(super) image: Arc<RenderImage>,
}

struct FileManagerSortedFilesCache {
    source_revision: u64,
    filter: String,
    sort_field: LocalSortField,
    sort_direction: LocalSortDirection,
    files: Arc<Vec<LocalFileEntry>>,
    rows: Arc<Vec<FileManagerListRow>>,
}

#[derive(Clone)]
struct FileManagerListRow {
    display_name: SharedString,
    size_text: SharedString,
    modified_text: SharedString,
    icon: LucideIcon,
    icon_color: u32,
}

impl FileManagerListRow {
    fn new(file: &LocalFileEntry) -> Self {
        // These values depend only on directory data, so compute them once instead
        // of repeating extension and local-time formatting on every scroll frame.
        let display_name = file
            .symlink_target
            .as_ref()
            .map(|target| format!("{} -> {target}", file.name))
            .unwrap_or_else(|| file.name.clone());
        let size_text = if file.file_type == LocalFileType::Directory {
            "-".to_string()
        } else {
            format_file_size(file.size)
        };
        let modified_text = format_modified(file.modified);
        let (icon, icon_color) = file_icon_for_entry(file);
        Self {
            display_name: display_name.into(),
            size_text: size_text.into(),
            modified_text: modified_text.into(),
            icon,
            icon_color,
        }
    }
}

pub(super) struct FileManagerState {
    pub(super) path: String,
    pub(super) path_input: String,
    pub(super) path_completion: PathCompletionState,
    pub(super) path_scroll: ScrollHandle,
    pub(super) editing_path: bool,
    pub(super) filter: String,
    pub(super) files: Vec<LocalFileEntry>,
    // Directory refreshes advance this revision so cached filtering and sorting never outlive data.
    source_revision: u64,
    sorted_files_cache: RefCell<Option<FileManagerSortedFilesCache>>,
    pub(super) loading: bool,
    pub(super) error: Option<String>,
    pub(super) selected: HashSet<String>,
    pub(super) last_selected: Option<String>,
    pub(super) sort_field: LocalSortField,
    pub(super) sort_direction: LocalSortDirection,
    pub(super) focused_input: Option<FileManagerInput>,
    pub(super) focused_dialog_footer_action: Option<ConfirmDialogAction>,
    pub(super) context_menu: Option<FileManagerContextMenu>,
    pub(super) context_menu_presence: oxideterm_gpui_ui::motion::ExitPresence,
    pub(super) context_menu_exit_generation: Option<u64>,
    pub(super) dialog: Option<FileManagerDialog>,
    pub(super) dialog_presence: oxideterm_gpui_ui::motion::ExitPresence,
    pub(super) dialog_value: String,
    pub(super) clipboard: Option<LocalClipboard>,
    pub(super) bookmarks: Vec<LocalBookmark>,
    pub(super) sidebar_locations: Vec<LocalSidebarLocation>,
    pub(super) drives: Vec<LocalDrive>,
    pub(super) bookmarks_path: PathBuf,
    pub(super) bookmarks_visible: bool,
    pub(super) list_scroll: UniformListScrollHandle,
    pub(super) preview: Option<LocalPreview>,
    pub(super) preview_metadata: Option<LocalPreviewMetadata>,
    pub(super) preview_show_metadata: bool,
    pub(super) preview_markdown_source: bool,
    pub(super) preview_image_zoom: f32,
    pub(super) preview_image_rotation: i32,
    pub(super) preview_rotated_image_cache: RefCell<Option<FileManagerRotatedPreviewImage>>,
    pub(super) preview_retired_images: RefCell<Vec<Arc<RenderImage>>>,
    pub(super) preview_code_scroll: UniformListScrollHandle,
    pub(super) preview_markdown_scroll: MarkdownVirtualListScrollHandle,
    pub(super) preview_archive_list_state: ListState,
    pub(super) preview_archive_list_cache: RefCell<VirtualListSignatureCache>,
    pub(super) preview_stream: FileManagerPreviewStreamState,
    pub(super) preview_audio: RodioAudioPreviewBackend,
    pub(super) preview_video_surface: SharedSftpNativeVideoSurface,
    pub(super) preview_font_family: Option<String>,
    pub(super) preview_font_error: Option<String>,
    pub(super) preview_font_size: f32,
    pub(super) operation_progress: Option<FileManagerOperationProgress>,
    pub(super) operation_rx: Option<std::sync::mpsc::Receiver<FileManagerOperationEvent>>,
    pub(super) operation_poll_active: bool,
    pub(super) properties_checksum: Option<LocalChecksumResult>,
    pub(super) properties_checksum_loading: bool,
    pub(super) properties_checksum_rx:
        Option<std::sync::mpsc::Receiver<Result<LocalChecksumResult, String>>>,
    pub(super) properties_checksum_poll_active: bool,
}

impl Default for FileManagerState {
    fn default() -> Self {
        let path = home_path();
        Self {
            path: path.clone(),
            path_input: path,
            path_completion: PathCompletionState::default(),
            path_scroll: ScrollHandle::new(),
            editing_path: false,
            filter: String::new(),
            files: Vec::new(),
            source_revision: 0,
            sorted_files_cache: RefCell::new(None),
            loading: false,
            error: None,
            selected: HashSet::new(),
            last_selected: None,
            sort_field: LocalSortField::Name,
            sort_direction: LocalSortDirection::Asc,
            focused_input: None,
            focused_dialog_footer_action: None,
            context_menu: None,
            context_menu_presence: oxideterm_gpui_ui::motion::ExitPresence::visible(),
            context_menu_exit_generation: None,
            dialog: None,
            dialog_presence: oxideterm_gpui_ui::motion::ExitPresence::visible(),
            dialog_value: String::new(),
            clipboard: None,
            bookmarks: Vec::new(),
            // Resolve system folders once so paint does not repeatedly query the filesystem.
            sidebar_locations: local_sidebar_locations(),
            // Disk discovery performs synchronous system and filesystem queries.
            // Cache it outside render and refresh only at explicit interaction boundaries.
            drives: local_drives(),
            bookmarks_path: default_file_manager_bookmarks_path(),
            bookmarks_visible: true,
            list_scroll: UniformListScrollHandle::new(),
            preview: None,
            preview_metadata: None,
            preview_show_metadata: true,
            preview_markdown_source: false,
            preview_image_zoom: 1.0,
            preview_image_rotation: 0,
            preview_rotated_image_cache: RefCell::new(None),
            preview_retired_images: RefCell::new(Vec::new()),
            preview_code_scroll: UniformListScrollHandle::new(),
            preview_markdown_scroll: MarkdownVirtualListScrollHandle::new(),
            // Archive previews can contain thousands of entries. Keep the file
            // rows on ListState instead of rebuilding the entire archive tree.
            preview_archive_list_state: ListState::new(
                FILE_MANAGER_ARCHIVE_LIST_INITIAL_ITEM_COUNT,
                ListAlignment::Top,
                TauriVirtualListSpec::new(
                    px(FILE_MANAGER_ARCHIVE_ROW_HEIGHT),
                    FILE_MANAGER_ARCHIVE_LIST_OVERSCAN,
                )
                .overdraw(),
            )
            .measure_all(),
            preview_archive_list_cache: RefCell::new(VirtualListSignatureCache::default()),
            preview_stream: FileManagerPreviewStreamState::default(),
            preview_audio: RodioAudioPreviewBackend::default(),
            preview_video_surface: SharedSftpNativeVideoSurface::default(),
            preview_font_family: None,
            preview_font_error: None,
            preview_font_size: 32.0,
            operation_progress: None,
            operation_rx: None,
            operation_poll_active: false,
            properties_checksum: None,
            properties_checksum_loading: false,
            properties_checksum_rx: None,
            properties_checksum_poll_active: false,
        }
    }
}

impl FileManagerState {
    fn sorted_files(&self) -> Arc<Vec<LocalFileEntry>> {
        if let Some(cache) = self.sorted_files_cache.borrow().as_ref()
            && cache.source_revision == self.source_revision
            && cache.filter == self.filter
            && cache.sort_field == self.sort_field
            && cache.sort_direction == self.sort_direction
        {
            return cache.files.clone();
        }

        let files = Arc::new(sorted_local_files(
            &self.files,
            &self.filter,
            self.sort_field,
            self.sort_direction,
        ));
        let rows = Arc::new(
            files
                .iter()
                .map(FileManagerListRow::new)
                .collect::<Vec<_>>(),
        );
        *self.sorted_files_cache.borrow_mut() = Some(FileManagerSortedFilesCache {
            source_revision: self.source_revision,
            filter: self.filter.clone(),
            sort_field: self.sort_field,
            sort_direction: self.sort_direction,
            files: files.clone(),
            rows,
        });
        files
    }

    fn sorted_file_rows(&self) -> Arc<Vec<FileManagerListRow>> {
        // Populate both aligned caches through the same validation path.
        let _ = self.sorted_files();
        self.sorted_files_cache
            .borrow()
            .as_ref()
            .expect("sorted file rows should exist after sorting")
            .rows
            .clone()
    }

    fn replace_files(&mut self, files: Vec<LocalFileEntry>) {
        // The revision keeps cache validation constant-time even when a directory is large.
        self.source_revision = self.source_revision.wrapping_add(1);
        self.files = files;
        self.sorted_files_cache.get_mut().take();
    }

    fn clear_files(&mut self) {
        self.replace_files(Vec::new());
    }

    pub(super) fn load(settings_path: &std::path::Path) -> Self {
        let bookmarks_path = settings_path
            .parent()
            .unwrap_or(settings_path)
            .join(FILE_MANAGER_BOOKMARKS_FILENAME);
        let mut state = Self {
            bookmarks_path,
            ..Self::default()
        };
        if let Ok(bytes) = std::fs::read(&state.bookmarks_path)
            && let Ok(bookmarks) = serde_json::from_slice::<Vec<LocalBookmark>>(&bytes)
        {
            state.bookmarks = bookmarks;
        }
        state
    }
}

fn file_manager_bg(color: u32, has_background: bool) -> Rgba {
    color_for_background(color, has_background, FILE_MANAGER_BG_ACTIVE_BG_ALPHA)
}

fn file_manager_panel_bg(color: u32, has_background: bool, alpha: u32) -> Rgba {
    color_with_background_scaled_alpha(
        color,
        has_background,
        alpha,
        FILE_MANAGER_BG_ACTIVE_PANEL_ALPHA,
    )
}

fn file_manager_hover_bg(color: u32, has_background: bool) -> Rgba {
    color_for_background(color, has_background, FILE_MANAGER_BG_ACTIVE_HOVER_ALPHA)
}

fn file_manager_border(color: u32, has_background: bool) -> Rgba {
    color_for_background(color, has_background, 0x99)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_entry(name: &str) -> LocalFileEntry {
        LocalFileEntry {
            name: name.to_string(),
            path: format!("/tmp/{name}"),
            file_type: LocalFileType::File,
            size: 0,
            modified: None,
            readonly: false,
            symlink_target: None,
        }
    }

    #[test]
    fn sorted_files_cache_reuses_results_until_the_query_changes() {
        let mut state = FileManagerState::default();
        state.replace_files(vec![cache_entry("beta"), cache_entry("alpha")]);

        // Unchanged render queries must reuse the same allocation.
        let initial = state.sorted_files();
        let reused = state.sorted_files();
        let initial_rows = state.sorted_file_rows();
        let reused_rows = state.sorted_file_rows();
        assert!(Arc::ptr_eq(&initial, &reused));
        assert!(Arc::ptr_eq(&initial_rows, &reused_rows));
        assert_eq!(initial_rows[0].display_name.as_ref(), "alpha");
        assert_eq!(initial_rows[0].size_text.as_ref(), "0 B");
        assert_eq!(initial_rows[0].modified_text.as_ref(), "-");

        // Filter changes invalidate the query without requiring an explicit mutation hook.
        state.filter = "beta".to_string();
        let filtered = state.sorted_files();
        let filtered_rows = state.sorted_file_rows();
        assert!(!Arc::ptr_eq(&initial, &filtered));
        assert!(!Arc::ptr_eq(&initial_rows, &filtered_rows));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "beta");

        // A directory refresh invalidates cached results even when the query is unchanged.
        state.replace_files(vec![cache_entry("beta"), cache_entry("beta-2")]);
        let refreshed = state.sorted_files();
        let refreshed_rows = state.sorted_file_rows();
        assert!(!Arc::ptr_eq(&filtered, &refreshed));
        assert!(!Arc::ptr_eq(&filtered_rows, &refreshed_rows));
        assert_eq!(refreshed.len(), 2);
    }
}

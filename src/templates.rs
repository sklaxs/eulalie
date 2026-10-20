use askama::Template;

// base.html n'est pas utilisé comme layout Askama (chaque page est autonome)

#[derive(Template)]
#[template(path = "login.html")]
pub struct LoginTemplate {
    pub error: Option<String>,
    pub mode_register: bool,
}

#[derive(Template)]
#[template(path = "projects.html")]
pub struct ProjectsTemplate {
    pub projects: Vec<ProjectRow>,
}

#[derive(sqlx::FromRow)]
pub struct ProjectRow {
    pub id: i64,
    pub title: String,
    pub source_kind: String,
    pub source_title: String,
    pub updated_at: String,
    pub chapter_count: i64,
    pub translated_count: i64,
}

#[derive(Template)]
#[template(path = "editor.html")]
pub struct EditorTemplate {
    pub project: ProjectMeta,
    pub chapters: Vec<ChapterMeta>,
    pub bookmarks: Vec<BookmarkMeta>,
    pub current: ChapterMeta,
    pub current_idx: usize,
    pub current_lines: Vec<String>,
    pub current_markdown: String,
    pub rendered_markdown: String,
    pub next_idx: Option<usize>,
    pub prev_idx: Option<usize>,
}

#[derive(sqlx::FromRow)]
pub struct ProjectMeta {
    pub id: i64,
    pub title: String,
    pub source_kind: String,
    pub source_title: String,
}

#[derive(sqlx::FromRow, Clone)]
pub struct ChapterMeta {
    pub id: i64,
    pub idx: i64,
    pub title: String,
    pub has_translation: bool,
}

#[derive(sqlx::FromRow)]
pub struct BookmarkMeta {
    pub id: i64,
    pub label: String,
    pub page: i64,
}

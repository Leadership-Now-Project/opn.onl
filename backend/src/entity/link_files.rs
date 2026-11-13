use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// `status` of an upload the browser has been given a URL for and has not
/// completed yet. It has no link.
pub const STATUS_PENDING: &str = "pending";
/// `status` of a file a short link serves.
pub const STATUS_READY: &str = "ready";

/// An uploaded file a short link serves. That link's `original_url` is a
/// marker, not a destination: see `links::Model::is_file`.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "link_files")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// `None` until the upload is completed, and again after the link row is
    /// purged (the foreign key is SET NULL so the object can still be found).
    pub link_id: Option<i32>,
    /// The uploader. Storage use is counted per uploader.
    pub user_id: Option<i32>,
    /// Where the object is in the bucket. Random, never built from the
    /// filename, and never sent to a client.
    pub storage_key: String,
    /// The name the uploader's file had, cleaned up (`files::clean_filename`).
    pub filename: String,
    /// What the uploader's browser called the file. Not trusted when serving:
    /// see `files::served_as`.
    pub content_type: String,
    pub size_bytes: i64,
    pub status: String,
    pub created_at: DateTime,
    /// When the object was removed from the bucket.
    pub deleted_at: Option<DateTime>,
    /// Secure View: the link opens the viewer (email first, then the file
    /// shown with deterrents) instead of redirecting to the file.
    #[sea_orm(default_value = "false")]
    pub secure_view: bool,
    /// Where the copy of a PDF stamped "no printing, no copying" is stored,
    /// when one was made. Served to the viewer in place of the original, and
    /// removed from the bucket with it. Never sent to a client.
    pub protected_key: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::links::Entity",
        from = "Column::LinkId",
        to = "super::links::Column::Id",
        on_update = "Cascade",
        on_delete = "SetNull"
    )]
    Link,
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::UserId",
        to = "super::users::Column::Id",
        on_update = "Cascade",
        on_delete = "SetNull"
    )]
    User,
}

impl Related<super::links::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Link.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

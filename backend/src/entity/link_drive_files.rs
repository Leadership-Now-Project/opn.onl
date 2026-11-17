use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// The Google Drive file a short link was made for. The link is an ordinary
/// URL link to the file's address in Drive; this row records which file that
/// is. It is removed when the link's destination changes, so it never
/// describes a different destination.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "link_drive_files")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub link_id: i32,
    /// Drive's id of the file.
    pub file_id: String,
    /// The file's name in Drive when it was picked.
    pub name: String,
    pub mime_type: String,
    /// Who picked the file.
    pub created_by: Option<i32>,
    pub created_at: DateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::links::Entity",
        from = "Column::LinkId",
        to = "super::links::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    Link,
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::CreatedBy",
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

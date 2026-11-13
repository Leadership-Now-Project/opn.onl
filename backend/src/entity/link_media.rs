use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// The video a media link shows. The link is an ordinary URL link to the
/// video's own page; this row says which video that is, and what the optional
/// button under the player says. It is removed when the link's destination
/// changes, so it never describes a different destination.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "link_media")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub link_id: i32,
    /// `youtube`, `vimeo` or `drive`.
    pub provider: String,
    /// The host's id of the video, as `utils::media` wrote it: never text a
    /// client sent as it stood.
    pub media_id: String,
    pub cta_label: Option<String>,
    pub cta_url: Option<String>,
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
}

impl Related<super::links::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Link.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

use crate::{
  activities::{
    deletion::{receive_delete_action, verify_delete_activity, DeletableObjects},
    generate_activity_id,
  },
  insert_activity,
  local_instance,
  objects::{community::ApubCommunity, person::ApubPerson},
  protocol::{activities::deletion::delete::Delete, IdOrNestedObject},
};
use activitypub_federation::{
  config::RequestData,
  fetch::object_id::ObjectId,
  kinds::activity::DeleteType,
  traits::{ActivityHandler, Actor},
};
use lemmy_api_common::{
  context::LemmyContext,
  websocket::{
    send::{send_comment_ws_message_simple, send_community_ws_message, send_post_ws_message},
    UserOperationCrud,
  },
};
use lemmy_db_schema::{
  source::{
    comment::{Comment, CommentUpdateForm},
    community::{Community, CommunityUpdateForm},
    moderator::{
      ModRemoveComment,
      ModRemoveCommentForm,
      ModRemoveCommunity,
      ModRemoveCommunityForm,
      ModRemovePost,
      ModRemovePostForm,
    },
    post::{Post, PostUpdateForm},
  },
  traits::Crud,
};
use lemmy_utils::error::LemmyError;
use url::Url;

#[async_trait::async_trait]
impl ActivityHandler for Delete {
  type DataType = LemmyContext;
  type Error = LemmyError;

  fn id(&self) -> &Url {
    &self.id
  }

  fn actor(&self) -> &Url {
    self.actor.inner()
  }

  #[tracing::instrument(skip_all)]
  async fn verify(&self, context: &RequestData<Self::DataType>) -> Result<(), LemmyError> {
    verify_delete_activity(self, self.summary.is_some(), context).await?;
    Ok(())
  }

  #[tracing::instrument(skip_all)]
  async fn receive(self, context: &RequestData<LemmyContext>) -> Result<(), LemmyError> {
    insert_activity(&self.id, &self, false, false, context).await?;
    if let Some(reason) = self.summary {
      // We set reason to empty string if it doesn't exist, to distinguish between delete and
      // remove. Here we change it back to option, so we don't write it to db.
      let reason = if reason.is_empty() {
        None
      } else {
        Some(reason)
      };
      receive_remove_action(
        &self.actor.dereference(context).await?,
        self.object.id(),
        reason,
        context,
      )
      .await
    } else {
      receive_delete_action(self.object.id(), &self.actor, true, context).await
    }
  }
}

impl Delete {
  pub(in crate::activities::deletion) fn new(
    actor: &ApubPerson,
    object: DeletableObjects,
    to: Url,
    community: Option<&Community>,
    summary: Option<String>,
    context: &LemmyContext,
  ) -> Result<Delete, LemmyError> {
    let id = generate_activity_id(
      DeleteType::Delete,
      &context.settings().get_protocol_and_hostname(),
    )?;
    let cc: Option<Url> = community.map(|c| c.actor_id.clone().into());
    Ok(Delete {
      actor: actor.actor_id.clone().into(),
      to: vec![to],
      object: IdOrNestedObject::Id(object.id()),
      cc: cc.into_iter().collect(),
      kind: DeleteType::Delete,
      summary,
      id,
      audience: community.map(|c| c.actor_id.clone().into()),
    })
  }
}

#[tracing::instrument(skip_all)]
pub(in crate::activities) async fn receive_remove_action(
  actor: &ApubPerson,
  object: &Url,
  reason: Option<String>,
  context: &LemmyContext,
) -> Result<(), LemmyError> {
  use UserOperationCrud::*;
  match DeletableObjects::read_from_db(object, context).await? {
    DeletableObjects::Community(community) => {
      if community.local {
        return Err(LemmyError::from_message(
          "Only local admin can remove community",
        ));
      }
      let form = ModRemoveCommunityForm {
        mod_person_id: actor.id,
        community_id: community.id,
        removed: Some(true),
        reason,
        expires: None,
      };
      ModRemoveCommunity::create(context.pool(), &form).await?;
      let deleted_community = Community::update(
        context.pool(),
        community.id,
        &CommunityUpdateForm::builder().removed(Some(true)).build(),
      )
      .await?;

      send_community_ws_message(deleted_community.id, RemoveCommunity, None, None, context).await?;
    }
    DeletableObjects::Post(post) => {
      let form = ModRemovePostForm {
        mod_person_id: actor.id,
        post_id: post.id,
        removed: Some(true),
        reason,
      };
      ModRemovePost::create(context.pool(), &form).await?;
      let removed_post = Post::update(
        context.pool(),
        post.id,
        &PostUpdateForm::builder().removed(Some(true)).build(),
      )
      .await?;

      send_post_ws_message(removed_post.id, RemovePost, None, None, context).await?;
    }
    DeletableObjects::Comment(comment) => {
      let form = ModRemoveCommentForm {
        mod_person_id: actor.id,
        comment_id: comment.id,
        removed: Some(true),
        reason,
      };
      ModRemoveComment::create(context.pool(), &form).await?;
      let removed_comment = Comment::update(
        context.pool(),
        comment.id,
        &CommentUpdateForm::builder().removed(Some(true)).build(),
      )
      .await?;

      send_comment_ws_message_simple(removed_comment.id, RemoveComment, context).await?;
    }
    DeletableObjects::PrivateMessage(_) => unimplemented!(),
  }
  Ok(())
}

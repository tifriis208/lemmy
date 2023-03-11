use crate::fetcher::post_or_comment::PostOrComment;
use activitypub_federation::{
  config::{FederationConfig, RequestData, UrlVerifier},
  traits::{Actor, ApubObject},
};
use async_trait::async_trait;
use lemmy_api_common::context::LemmyContext;
use lemmy_db_schema::{
  source::{
    activity::{Activity, ActivityInsertForm},
    instance::Instance,
    local_site::LocalSite,
  },
  traits::Crud,
  utils::DbPool,
};
use lemmy_utils::{error::LemmyError, settings::structs::Settings};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use url::Url;

pub mod activities;
pub(crate) mod activity_lists;
pub mod api;
pub(crate) mod collections;
pub mod fetcher;
pub mod http;
pub(crate) mod mentions;
pub mod objects;
pub mod protocol;

const FEDERATION_HTTP_FETCH_LIMIT: i32 = 25;

static CONTEXT: Lazy<Vec<serde_json::Value>> = Lazy::new(|| {
  serde_json::from_str(include_str!("../assets/lemmy/context.json")).expect("parse context")
});

// TODO: store this in context? but its only used in this crate, no need to expose it elsewhere
// TODO this singleton needs to be redone to account for live data.
async fn local_instance(context: &LemmyContext) -> &'static FederationConfig<LemmyContext> {
  static LOCAL_INSTANCE: OnceCell<FederationConfig<LemmyContext>> = OnceCell::const_new();
  LOCAL_INSTANCE
    .get_or_init(|| async {
      // Local site may be missing
      let local_site = &LocalSite::read(context.pool()).await;
      let worker_count = local_site
        .as_ref()
        .map(|l| l.federation_worker_count)
        .unwrap_or(64) as u64;

      FederationConfig::builder()
        .domain(context.settings().hostname.clone())
        .app_data(context.clone())
        .client(context.client().clone())
        .http_fetch_limit(FEDERATION_HTTP_FETCH_LIMIT)
        .worker_count(worker_count)
        .debug(cfg!(debug_assertions))
        .http_signature_compat(true)
        .url_verifier(Box::new(VerifyUrlData(context.clone())))
        .build()
        .expect("configure federation")
    })
    .await
}

#[derive(Clone)]
struct VerifyUrlData(LemmyContext);

#[async_trait]
impl UrlVerifier for VerifyUrlData {
  async fn verify(&self, url: &Url) -> Result<(), &'static str> {
    let local_site_data = fetch_local_site_data(self.0.pool())
      .await
      .expect("read local site data");
    check_apub_id_valid(url, &local_site_data, self.0.settings())
  }
}

/// Checks if the ID is allowed for sending or receiving.
///
/// In particular, it checks for:
/// - federation being enabled (if its disabled, only local URLs are allowed)
/// - the correct scheme (either http or https)
/// - URL being in the allowlist (if it is active)
/// - URL not being in the blocklist (if it is active)
///
/// `use_strict_allowlist` should be true only when parsing a remote community, or when parsing a
/// post/comment in a local community.
#[tracing::instrument(skip(settings, local_site_data))]
fn check_apub_id_valid(
  apub_id: &Url,
  local_site_data: &LocalSiteData,
  settings: &Settings,
) -> Result<(), &'static str> {
  let domain = apub_id.domain().expect("apud id has domain").to_string();
  let local_instance = settings
    .get_hostname_without_port()
    .expect("local hostname is valid");
  if domain == local_instance {
    return Ok(());
  }

  if !local_site_data
    .local_site
    .as_ref()
    .map(|l| l.federation_enabled)
    .unwrap_or(true)
  {
    return Err("Federation disabled");
  }

  if apub_id.scheme() != settings.get_protocol_string() {
    return Err("Invalid protocol scheme");
  }

  if let Some(blocked) = local_site_data.blocked_instances.as_ref() {
    if blocked.iter().any(|i| domain.eq(&i.domain)) {
      return Err("Domain is blocked");
    }
  }

  if let Some(allowed) = local_site_data.allowed_instances.as_ref() {
    if !allowed.iter().any(|i| domain.eq(&i.domain)) {
      return Err("Domain is not in allowlist");
    }
  }

  Ok(())
}

#[derive(Clone)]
pub(crate) struct LocalSiteData {
  local_site: Option<LocalSite>,
  allowed_instances: Option<Vec<Instance>>,
  blocked_instances: Option<Vec<Instance>>,
}

pub(crate) async fn fetch_local_site_data(
  pool: &DbPool,
) -> Result<LocalSiteData, diesel::result::Error> {
  // LocalSite may be missing
  let local_site = LocalSite::read(pool).await.ok();
  let allowed = Instance::allowlist(pool).await?;
  let blocked = Instance::blocklist(pool).await?;

  // These can return empty vectors, so convert them to options
  let allowed_instances = (!allowed.is_empty()).then_some(allowed);
  let blocked_instances = (!blocked.is_empty()).then_some(blocked);

  Ok(LocalSiteData {
    local_site,
    allowed_instances,
    blocked_instances,
  })
}

#[tracing::instrument(skip(settings, local_site_data))]
pub(crate) fn check_apub_id_valid_with_strictness(
  apub_id: &Url,
  is_strict: bool,
  local_site_data: &LocalSiteData,
  settings: &Settings,
) -> Result<(), LemmyError> {
  check_apub_id_valid(apub_id, local_site_data, settings).map_err(LemmyError::from_message)?;
  let domain = apub_id.domain().expect("apud id has domain").to_string();
  let local_instance = settings
    .get_hostname_without_port()
    .expect("local hostname is valid");
  if domain == local_instance {
    return Ok(());
  }

  if let Some(allowed) = local_site_data.allowed_instances.as_ref() {
    // Only check allowlist if this is a community
    if is_strict {
      // need to allow this explicitly because apub receive might contain objects from our local
      // instance.
      let mut allowed_and_local = allowed
        .iter()
        .map(|i| i.domain.clone())
        .collect::<Vec<String>>();
      allowed_and_local.push(local_instance);

      if !allowed_and_local.contains(&domain) {
        return Err(LemmyError::from_message(
          "Federation forbidden by strict allowlist",
        ));
      }
    }
  }
  Ok(())
}

/// Store a sent or received activity in the database.
///
/// Stored activities are served over the HTTP endpoint `GET /activities/{type_}/{id}`. This also
/// ensures that the same activity cannot be received more than once.
#[tracing::instrument(skip(data, activity))]
async fn insert_activity<T>(
  ap_id: &Url,
  activity: &T,
  local: bool,
  sensitive: bool,
  data: &RequestData<LemmyContext>,
) -> Result<(), LemmyError>
where
  T: Serialize,
{
  let ap_id = ap_id.clone().into();
  let form = ActivityInsertForm {
    ap_id,
    data: serde_json::to_value(activity)?,
    local: Some(local),
    sensitive: Some(sensitive),
    updated: None,
  };
  Activity::create(data.pool(), &form).await?;
  Ok(())
}

#[async_trait::async_trait]
pub trait SendActivity: Sync {
  type Response: Sync + Send;

  async fn send_activity(
    _request: &Self,
    _response: &Self::Response,
    _context: &LemmyContext,
  ) -> Result<(), LemmyError> {
    Ok(())
  }
}

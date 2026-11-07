//! CLI delivery input: upload demo/evidence files first, then append one
//! `delivery` event with BlobRefs only.

use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use agendar_core::error::{AgendarError, ErrorCode, Result};
use agendar_core::model::{
    Agent, BlobExt, BlobRef, CheckResult, Claim, CliTranscriptDemo, CriterionId, CriterionResult,
    Delivery, Demo, DemoCheck, DemoKind, Disclosure, EventId, ProjectSlug, ScreenshotDemo,
    SessionId,
};
use agendar_core::runtime::checks::{CheckProfile, NormalizedProfile, profile_digest};
use agendar_core::service::Service;
use agendar_core::vault::blobs::{put_blob, read_blob};

/// Parse the supported attachment extension subset.
pub fn parse_blob_ext(value: &str) -> Result<BlobExt> {
    match value {
        "txt" => Ok(BlobExt::Txt),
        "md" => Ok(BlobExt::Md),
        "html" => Ok(BlobExt::Html),
        "png" => Ok(BlobExt::Png),
        "jpg" => Ok(BlobExt::Jpg),
        "webp" => Ok(BlobExt::Webp),
        "svg" => Ok(BlobExt::Svg),
        "mp4" | "webm" => Err(AgendarError::new(
            ErrorCode::Unsupported,
            "Video attachments are not supported in this version.",
        )
        .with_detail("blob_ext_video_unsupported")
        .with_hint("Use png, jpg, webp, txt, md or html.")),
        other => Err(AgendarError::validation_at(
            "/ext",
            "blob_ext",
            format!("Unknown attachment extension `{other}`."),
        )
        .with_hint("Use png, jpg, webp, txt, md or html.")),
    }
}

/// Disclosure input. The wire body uses `not_proven_yet`/`faked_for_now`; the
/// short `not_proven`/`faked` spellings are accepted as input aliases.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisclosureInput {
    #[serde(default)]
    pub proven: Vec<String>,
    #[serde(default)]
    pub not_proven_yet: Option<Vec<String>>,
    #[serde(default)]
    pub not_proven: Option<Vec<String>>,
    #[serde(default)]
    pub faked_for_now: Option<Vec<String>>,
    #[serde(default)]
    pub faked: Option<Vec<String>>,
}

impl DisclosureInput {
    fn into_disclosure(self) -> Result<Disclosure> {
        let not_proven_yet = self.not_proven_yet.or(self.not_proven).unwrap_or_default();
        let faked_for_now = self.faked_for_now.or(self.faked).unwrap_or_default();
        for (field, values) in [
            ("/disclosure/proven", &self.proven),
            ("/disclosure/not_proven_yet", &not_proven_yet),
            ("/disclosure/faked_for_now", &faked_for_now),
        ] {
            if values.len() > 10 {
                return Err(AgendarError::validation_at(
                    field,
                    "disclosure_count",
                    "Each disclosure column holds at most ten statements.",
                ));
            }
            if values
                .iter()
                .any(|value| value.trim().is_empty() || value.chars().count() > 240)
            {
                return Err(AgendarError::validation_at(
                    field,
                    "disclosure_text",
                    "Disclosure statements must be 1..=240 characters.",
                ));
            }
        }
        Ok(Disclosure {
            proven: self.proven,
            not_proven_yet,
            faked_for_now,
        })
    }
}

/// One attachment input: a local file to upload or an existing Vault blob.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobInput {
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub ext: Option<BlobExt>,
    #[serde(default)]
    pub blob: Option<BlobRef>,
    #[serde(default)]
    pub alt: Option<String>,
    #[serde(default)]
    pub check: Option<DemoCheck>,
}

impl BlobInput {
    fn exactly_one(&self, field: &str) -> Result<()> {
        match (&self.file, &self.blob) {
            (Some(_), None) | (None, Some(_)) => Ok(()),
            _ => Err(AgendarError::validation_at(
                field,
                "blob_input_shape",
                "Each attachment must contain exactly one of `file` or `blob`.",
            )),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemoInput {
    pub kind: DemoKind,
    pub body: BlobInput,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionInput {
    pub criterion: CriterionId,
    pub status: CheckResult,
    #[serde(default)]
    pub claim: Option<Claim>,
    #[serde(default)]
    pub evidence: Option<BlobInput>,
    #[serde(default)]
    pub observation: Option<EventId>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckProposalInput {
    pub criterion: CriterionId,
    pub argv: Vec<String>,
    pub cwd: String,
    #[serde(default)]
    pub timeout_s: Option<u32>,
}

impl CheckProposalInput {
    fn profile(&self) -> CheckProfile {
        CheckProfile {
            id: None,
            argv: self.argv.clone(),
            args: Vec::new(),
            executable: None,
            cwd: Some(self.cwd.clone()),
            repo_root: None,
            timeout_s: self.timeout_s,
            delivery_id: None,
            criterion: Some(self.criterion.clone()),
        }
    }
}

/// The complete `--json DeliveryInput` object.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryInput {
    pub name: String,
    pub demos: Vec<DemoInput>,
    pub criteria: Vec<CriterionInput>,
    pub disclosure: DisclosureInput,
    #[serde(default)]
    pub checks: Vec<CheckProposalInput>,
}

/// Publish one local file or verify one existing blob.
pub fn upload_blob(
    service: &Service,
    project: &ProjectSlug,
    input: &BlobInput,
    field: &str,
    allowed: &[BlobExt],
) -> Result<BlobRef> {
    input.exactly_one(field)?;
    if let Some(blob) = &input.blob {
        blob.validate(field)?;
        if !allowed.contains(&blob.ext) {
            return Err(AgendarError::validation_at(
                format!("{field}/ext"),
                "blob_ext_not_allowed",
                format!(
                    "This attachment type must be one of {}.",
                    allowed
                        .iter()
                        .map(|ext| ext.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        // Referencing an already published blob is valid only when this host
        // can verify the exact bytes; a bare digest is not evidence.
        read_blob(service.vault(), project, blob)?;
        return Ok(blob.clone());
    }
    let file = input.file.as_deref().unwrap_or_default();
    if file.is_empty() {
        return Err(AgendarError::validation_at(
            format!("{field}/file"),
            "blob_input_file",
            "The attachment file path must not be blank.",
        ));
    }
    let ext = input.ext.ok_or_else(|| {
        AgendarError::validation_at(
            format!("{field}/ext"),
            "blob_input_ext",
            "A local attachment requires an explicit ext.",
        )
    })?;
    if !allowed.contains(&ext) {
        return Err(AgendarError::validation_at(
            format!("{field}/ext"),
            "blob_ext_not_allowed",
            format!(
                "This attachment type must be one of {}.",
                allowed
                    .iter()
                    .map(|ext| ext.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    upload_file(service, project, Path::new(file), ext)
}

fn upload_file(
    service: &Service,
    project: &ProjectSlug,
    path: &Path,
    ext: BlobExt,
) -> Result<BlobRef> {
    let canonical = path.canonicalize().map_err(|error| {
        AgendarError::new(
            ErrorCode::Validation,
            format!("Cannot read the attachment file: {error}."),
        )
        .with_field("/file")
        .with_detail("attachment_missing")
    })?;
    let metadata = fs::metadata(&canonical)?;
    if !metadata.is_file() {
        return Err(AgendarError::validation_at(
            "/file",
            "attachment_not_file",
            "An attachment must be a regular file.",
        ));
    }
    let mut handle = fs::File::open(&canonical)?;
    put_blob(
        service.vault(),
        service.local(),
        project,
        service.host(),
        ext,
        &mut handle,
    )
}

const IMAGE_EXTS: [BlobExt; 3] = [BlobExt::Png, BlobExt::Jpg, BlobExt::Webp];
const TEXT_EXTS: [BlobExt; 3] = [BlobExt::Txt, BlobExt::Md, BlobExt::Html];
const TRANSCRIPT_EXTS: [BlobExt; 3] = [BlobExt::Txt, BlobExt::Md, BlobExt::Html];

/// Build the immutable event body after every input file was published.
pub fn build_body(
    service: &Service,
    project: &ProjectSlug,
    session: &SessionId,
    harness: agendar_core::model::Harness,
    input: &DeliveryInput,
) -> Result<(Delivery, Vec<BlobRef>)> {
    if input.name.trim().is_empty() || input.name.trim().chars().count() > 80 {
        return Err(AgendarError::validation_at(
            "/name",
            "delivery_name",
            "A delivery name must be 1..=80 characters.",
        ));
    }
    if input.demos.is_empty() || input.demos.len() > 3 {
        return Err(AgendarError::validation_at(
            "/demos",
            "delivery_demos",
            "A delivery must contain 1..=3 demos.",
        ));
    }
    let mut uploaded = Vec::new();
    let mut demos = Vec::new();
    let criteria_ids: Vec<CriterionId> = input
        .criteria
        .iter()
        .map(|criterion| criterion.criterion.clone())
        .collect();
    let mut config: Option<agendar_core::runtime::Config> = None;
    for (index, demo) in input.demos.iter().enumerate() {
        let field = format!("/demos/{index}/body");
        let check = match &demo.body.check {
            Some(raw) => Some(canonical_demo_check(
                service,
                project,
                raw,
                &criteria_ids,
                &mut config,
            )?),
            None => None,
        };
        match demo.kind {
            DemoKind::Screenshot => {
                let blob = upload_blob(service, project, &demo.body, &field, &IMAGE_EXTS)?;
                let alt = demo.body.alt.clone().ok_or_else(|| {
                    AgendarError::validation_at(
                        format!("/demos/{index}/body/alt"),
                        "screenshot_alt",
                        "A screenshot demo requires alt text.",
                    )
                })?;
                if alt.trim().is_empty() || alt.trim().chars().count() > 160 {
                    return Err(AgendarError::validation_at(
                        format!("/demos/{index}/body/alt"),
                        "screenshot_alt",
                        "Screenshot alt text must be 1..=160 characters.",
                    ));
                }
                uploaded.push(blob.clone());
                demos.push(Demo::Screenshot(ScreenshotDemo { blob, alt, check }));
            }
            DemoKind::CliTranscript => {
                let blob = upload_blob(service, project, &demo.body, &field, &TRANSCRIPT_EXTS)?;
                uploaded.push(blob.clone());
                demos.push(Demo::CliTranscript(CliTranscriptDemo { blob, check }));
            }
            DemoKind::PreviewUrl | DemoKind::Recording => {
                return Err(AgendarError::new(
                    ErrorCode::Unsupported,
                    "This demo kind is not implemented in this version.",
                )
                .with_detail("demo_kind_unsupported")
                .with_hint("Use a screenshot or CLI transcript demo."));
            }
        }
    }
    if input.criteria.is_empty() || input.criteria.len() > 8 {
        return Err(AgendarError::validation_at(
            "/criteria",
            "delivery_criteria",
            "A delivery must contain 1..=8 criteria.",
        ));
    }
    let mut criteria = Vec::new();
    for (index, criterion) in input.criteria.iter().enumerate() {
        let claim = criterion.claim.unwrap_or(Claim::AgentSays);
        if claim != Claim::AgentSays {
            return Err(AgendarError::validation_at(
                format!("/criteria/{index}/claim"),
                "delivery_seen_requires_later_observation",
                "Delivery criteria must claim agent_says and omit observation.",
            ));
        }
        if criterion.observation.is_some() {
            return Err(AgendarError::validation_at(
                format!("/criteria/{index}/observation"),
                "delivery_observation_forbidden",
                "Delivery criteria must omit observation.",
            ));
        }
        let evidence = match &criterion.evidence {
            Some(evidence) => {
                let blob = upload_blob(
                    service,
                    project,
                    evidence,
                    &format!("/criteria/{index}/evidence"),
                    &TEXT_EXTS,
                )?;
                uploaded.push(blob.clone());
                Some(blob)
            }
            None => None,
        };
        criteria.push(CriterionResult {
            criterion: criterion.criterion.clone(),
            status: criterion.status,
            claim,
            evidence,
            observation: None,
        });
    }
    let body = Delivery {
        agent: Agent {
            session: session.clone(),
            harness,
        },
        name: input.name.clone(),
        demos,
        criteria,
        disclosure: input.disclosure.clone().into_disclosure()?,
    };
    Ok((body, uploaded))
}

/// Validate proposed checks without running anything, and optionally persist
/// them once the delivery event id exists. The CLI never executes a check; the
/// app requires a separate human approval of the exact profile.
pub fn validate_proposals(
    service: &Service,
    project: &ProjectSlug,
    delivery: Option<&EventId>,
    input: &DeliveryInput,
) -> Result<()> {
    if input.checks.is_empty() {
        return Ok(());
    }
    let config = agendar_core::runtime::Config::load_or_default(service.local().data_root())?;
    for (index, proposal) in input.checks.iter().enumerate() {
        let normalized: NormalizedProfile = proposal.profile().normalize().map_err(|error| {
            AgendarError::validation_at(
                format!("/checks/{index}"),
                "check_proposal",
                error.message(),
            )
        })?;
        agendar_core::runtime::checks::validate_cwd(
            &normalized.cwd,
            &config,
            Some(project.as_str()),
        )?;
        let digest = profile_digest(&normalized);
        if let Some(delivery) = delivery {
            agendar_core::runtime::checks::store_proposal(
                service.local(),
                service.vault(),
                delivery,
                &proposal.criterion,
                &normalized,
                &digest,
            )?;
        }
    }
    Ok(())
}

/// Canonicalize a proposed check's cwd and validate that it is inside a linked
/// repository for this project. Nothing is executed.
fn canonical_demo_check(
    service: &Service,
    project: &ProjectSlug,
    raw: &DemoCheck,
    criteria: &[CriterionId],
    config: &mut Option<agendar_core::runtime::Config>,
) -> Result<DemoCheck> {
    if !criteria.contains(&raw.criterion) {
        return Err(AgendarError::validation_at(
            "/demos/check/criterion",
            "check_criterion",
            "A proposed check must address one of the delivery's criteria.",
        ));
    }
    let profile = CheckProfile {
        id: None,
        argv: raw.argv.clone(),
        args: Vec::new(),
        executable: None,
        cwd: Some(raw.cwd.clone()),
        repo_root: None,
        timeout_s: Some(raw.timeout_s),
        delivery_id: None,
        criterion: Some(raw.criterion.clone()),
    };
    let normalized = profile.normalize()?;
    if config.is_none() {
        *config = Some(agendar_core::runtime::Config::load_or_default(
            service.local().data_root(),
        )?);
    }
    let cwd = if let Some(config) = config.as_ref() {
        agendar_core::runtime::checks::validate_cwd(
            &normalized.cwd,
            config,
            Some(project.as_str()),
        )?
    } else {
        return Err(AgendarError::new(
            ErrorCode::Context,
            "Local repository links are not available.",
        ));
    };
    Ok(DemoCheck {
        criterion: raw.criterion.clone(),
        argv: raw.argv.clone(),
        cwd: cwd.to_string_lossy().into_owned(),
        timeout_s: raw.timeout_s,
    })
}

/// The `blob put` JSON result.
pub fn blob_put_result(blob: &BlobRef) -> serde_json::Value {
    json!({"blob": blob})
}

/// One `options[].image` input on an ask card: a local file to publish or a
/// blob already published with `agendar blob put`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageInput {
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    ext: Option<BlobExt>,
    #[serde(default)]
    blob: Option<BlobRef>,
}

/// The card without its option images, for validating everything else before
/// any file is published.
pub fn without_option_images(card: &Value) -> Value {
    let mut bare = card.clone();
    if let Some(options) = bare.get_mut("options").and_then(Value::as_array_mut) {
        for option in options {
            if let Some(option) = option.as_object_mut() {
                option.remove("image");
            }
        }
    }
    bare
}

/// Publish each option image and replace its input with the BlobRef.
pub fn resolve_option_images(
    service: &Service,
    project: &ProjectSlug,
    card: &mut Value,
) -> Result<()> {
    let Some(options) = card.get_mut("options").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for (index, option) in options.iter_mut().enumerate() {
        let Some(slot) = option
            .as_object_mut()
            .and_then(|option| option.get_mut("image"))
        else {
            continue;
        };
        let field = format!("/card/options/{index}/image");
        let input: ImageInput = serde_json::from_value(slot.clone()).map_err(|error| {
            AgendarError::validation_at(
                field.clone(),
                "option_image_input",
                format!(
                    "An option image is {{\"file\": \"<path>\", \"ext\": \"png\"}} or {{\"blob\": ...}} from blob put: {error}."
                ),
            )
        })?;
        let input = BlobInput {
            file: input.file,
            ext: input.ext,
            blob: input.blob,
            alt: None,
            check: None,
        };
        let blob = upload_blob(service, project, &input, &field, &IMAGE_EXTS)?;
        *slot = serde_json::to_value(&blob).map_err(|error| {
            AgendarError::new(
                ErrorCode::Io,
                format!("Cannot encode the option image reference: {error}."),
            )
        })?;
    }
    Ok(())
}

/// Publish a project declaration's `icon` input (`{"file", "ext"}` or a
/// `{"blob"}` from `blob put`) and replace it with the BlobRef.
pub fn resolve_project_icon(
    service: &Service,
    project: &ProjectSlug,
    patch: &mut Value,
) -> Result<()> {
    let Some(slot) = patch
        .as_object_mut()
        .and_then(|patch| patch.get_mut("icon"))
    else {
        return Ok(());
    };
    let input: ImageInput = serde_json::from_value(slot.clone()).map_err(|error| {
        AgendarError::validation_at(
            "/icon",
            "project_icon_input",
            format!(
                "A project icon is {{\"file\": \"<path>\", \"ext\": \"png\"}} or {{\"blob\": ...}} from blob put: {error}."
            ),
        )
    })?;
    let input = BlobInput {
        file: input.file,
        ext: input.ext,
        blob: input.blob,
        alt: None,
        check: None,
    };
    let blob = upload_blob(
        service,
        project,
        &input,
        "/icon",
        &agendar_core::model::validate::ICON_EXTS,
    )?;
    *slot = serde_json::to_value(&blob).map_err(|error| {
        AgendarError::new(
            ErrorCode::Io,
            format!("Cannot encode the project icon reference: {error}."),
        )
    })?;
    Ok(())
}

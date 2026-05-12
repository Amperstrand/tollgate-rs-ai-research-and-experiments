use nostr::event::tag::{Tag, TagKind};
use nostr::prelude::*;

use super::{CustomerSession, V1ServerConfig};

#[derive(Debug, thiserror::Error)]
pub enum AllotmentError {
    #[error("no accepted mint found for: {0}")]
    UnknownMint(String),
    #[error("insufficient steps: got {steps}, minimum is {min_steps}")]
    InsufficientSteps { steps: u64, min_steps: u64 },
}

pub fn build_advertisement(
    config: &V1ServerConfig,
) -> Result<String, nostr::event::builder::Error> {
    let mut tags: Vec<Tag> = vec![
        Tag::custom(
            TagKind::Custom("metric".into()),
            [config.metric.clone()],
        ),
        Tag::custom(
            TagKind::Custom("step_size".into()),
            [config.step_size.to_string()],
        ),
    ];

    for mint in &config.accepted_mints {
        tags.push(Tag::custom(
            TagKind::Custom("price_per_step".into()),
            [
                "cashu".to_owned(),
                mint.price_per_step.to_string(),
                mint.unit.clone(),
                mint.url.clone(),
                mint.min_steps.to_string(),
            ],
        ));
    }

    let event = EventBuilder::new(Kind::Custom(10_021), "")
        .tags(Tags::from_list(tags))
        .sign_with_keys(&config.nostr_keys)?;

    Ok(event.as_json())
}

pub fn calculate_allotment(
    amount_sats: u64,
    mint_url: &str,
    config: &V1ServerConfig,
) -> Result<u64, AllotmentError> {
    let mint = config
        .accepted_mints
        .iter()
        .find(|m| m.url == mint_url)
        .ok_or_else(|| AllotmentError::UnknownMint(mint_url.to_owned()))?;

    if mint.price_per_step == 0 {
        return Ok(config.step_size);
    }

    let steps = amount_sats / mint.price_per_step;
    if steps < mint.min_steps {
        return Err(AllotmentError::InsufficientSteps {
            steps,
            min_steps: mint.min_steps,
        });
    }

    Ok(steps * config.step_size)
}

pub fn build_session_event(
    session: &CustomerSession,
    config: &V1ServerConfig,
) -> Result<String, nostr::event::builder::Error> {
    let tags = Tags::from_list(vec![
        Tag::custom(
            TagKind::Custom("allotment".into()),
            [session.allotment.to_string()],
        ),
        Tag::custom(
            TagKind::Custom("metric".into()),
            [session.metric.clone()],
        ),
        Tag::custom(
            TagKind::Custom("device-identifier".into()),
            ["mac".to_owned(), session.mac_address.clone()],
        ),
    ]);

    let event = EventBuilder::new(Kind::Custom(1022), "")
        .tags(tags)
        .sign_with_keys(&config.nostr_keys)?;

    Ok(event.as_json())
}

pub fn build_notice_event(
    level: &str,
    code: &str,
    message: &str,
    config: &V1ServerConfig,
) -> Result<String, nostr::event::builder::Error> {
    let tags = Tags::from_list(vec![
        Tag::custom(TagKind::Custom("level".into()), [level.to_owned()]),
        Tag::custom(TagKind::Custom("code".into()), [code.to_owned()]),
    ]);

    let event = EventBuilder::new(Kind::Custom(21_023), message)
        .tags(tags)
        .sign_with_keys(&config.nostr_keys)?;

    Ok(event.as_json())
}

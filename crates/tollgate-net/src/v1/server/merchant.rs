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
        Tag::custom(TagKind::Custom("metric".into()), [config.metric.clone()]),
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

    tags.push(Tag::custom(
        TagKind::Custom("tips".into()),
        ["1", "2", "3", "4"],
    ));

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
    customer_identifier: &str,
    amount_sat: u64,
    token_type: &str,
) -> Result<String, nostr::event::builder::Error> {
    let mut tags: Vec<Tag> = vec![
        Tag::custom(
            TagKind::Custom("p".into()),
            [customer_identifier.to_owned()],
        ),
        Tag::custom(
            TagKind::Custom("allotment".into()),
            [session.allotment.to_string()],
        ),
        Tag::custom(TagKind::Custom("metric".into()), [session.metric.clone()]),
        Tag::custom(
            TagKind::Custom("device-identifier".into()),
            ["mac".to_owned(), session.mac_address.clone()],
        ),
        Tag::custom(
            TagKind::Custom("start-time".into()),
            [session.start_time.to_string()],
        ),
        Tag::custom(
            TagKind::Custom("amount_sat".into()),
            [amount_sat.to_string()],
        ),
        Tag::custom(
            TagKind::Custom("token_type".into()),
            [token_type.to_owned()],
        ),
    ];

    if amount_sat > 0 {
        let effective_rate = session.allotment / 1000 / amount_sat;
        tags.push(Tag::custom(
            TagKind::Custom("effective_rate".into()),
            [effective_rate.to_string()],
        ));
    }

    let event = EventBuilder::new(Kind::Custom(1022), "")
        .tags(Tags::from_list(tags))
        .sign_with_keys(&config.nostr_keys)?;

    Ok(event.as_json())
}

pub fn build_notice_event(
    level: &str,
    code: &str,
    message: &str,
    customer_identifier: Option<&str>,
    config: &V1ServerConfig,
) -> Result<String, nostr::event::builder::Error> {
    let mut tags: Vec<Tag> = vec![
        Tag::custom(TagKind::Custom("level".into()), [level.to_owned()]),
        Tag::custom(TagKind::Custom("code".into()), [code.to_owned()]),
    ];

    // Go v1 parity: include p tag when customer pubkey/MAC is available
    if let Some(id) = customer_identifier {
        if !id.is_empty() {
            tags.push(Tag::custom(TagKind::Custom("p".into()), [id.to_owned()]));
        }
    }

    let event = EventBuilder::new(Kind::Custom(21_023), message)
        .tags(Tags::from_list(tags))
        .sign_with_keys(&config.nostr_keys)?;

    Ok(event.as_json())
}

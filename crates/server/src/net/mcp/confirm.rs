//! La confirmation humaine côté outils (étape E3). Un outil qui change quelque chose calcule d'abord
//! ce qu'il ferait (un [`Plan`]), puis passe par [`gate`] :
//!
//! - rien de sensible et pas d'`confirmation_id` : on y va ;
//! - quelque chose de sensible : **rien n'est exécuté**, une demande part à la personne propriétaire
//!   de l'agent (carte « Demandes à valider » de l'app web et notification, texte générique) et
//!   l'agent reçoit `confirmation_required` avec un identifiant ;
//! - l'agent rappelle le **même** outil avec ce `confirmation_id` : on exécute seulement si la
//!   personne a autorisé **cette** demande, pour **ce** jeton, avec la **même empreinte** (outil,
//!   arguments, et ce que le serveur en a déduit : appareils visés, mode de départ, règles qui
//!   partiraient), dans les 5 minutes, une seule fois.
//!
//! Un `confirmation_id` donné est toujours vérifié, jamais ignoré. Chaque contrôle d'un outil (droits,
//! appareils permis, liste « jamais ») est refait à chaque appel : une autorisation n'en contourne aucun.

use serde_json::{json, Map, Value};

use super::tools::{name_of, Ctx, ToolError};
use crate::audit::{Action, Entry, Origin};
use crate::confirmations::{self, ConsumeError, Draft, RequestError, State, Ticket};
use crate::events::now_unix;
use crate::push::content;
use crate::push::Category;
use crate::rights::Capability;

/// Pourquoi une personne doit valider : une liste fermée de motifs, chacun avec sa phrase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reason {
    /// L'appareil est marqué « sensible » (porte de garage, baie vitrée).
    Sensitive,
    /// La commande ferait partir une automatisation qui change le mode, appelle une adresse web ou
    /// commande un appareil sensible, protégé ou que cet agent ne peut pas commander.
    Rules,
    /// Une caméra qui surveille cesserait de surveiller.
    Disarm,
    /// Une caméra intérieure se mettrait à filmer alors que quelqu'un est (ou peut être) là.
    Privacy,
    /// Le mode « Vacances » dit que la maison est vide : toujours une confirmation.
    Vacances,
}

impl Reason {
    pub(super) fn word(self) -> &'static str {
        match self {
            Reason::Sensitive => "sensitive_device",
            Reason::Rules => "automation",
            Reason::Disarm => "disarm",
            Reason::Privacy => "privacy",
            Reason::Vacances => "vacances",
        }
    }

    fn phrase(self) -> &'static str {
        match self {
            Reason::Sensitive => "Un des appareils est marqué « sensible ».",
            Reason::Rules => "Cela ferait partir des automatisations qui changent le mode, appellent une adresse web ou commandent un appareil protégé.",
            Reason::Disarm => "Une caméra qui surveille cesserait de surveiller.",
            Reason::Privacy => "Une caméra intérieure se mettrait à filmer alors que quelqu'un est à la maison (ou que la présence est inconnue).",
            Reason::Vacances => "Le mode Vacances dit que la maison est vide.",
        }
    }
}

/// Ce qu'un outil ferait, et s'il doit attendre une personne.
pub(super) struct Plan {
    /// Le nom court de l'outil (`set_mode`…), mot fermé : il part au journal.
    pub tool: &'static str,
    pub capability: Capability,
    /// La phrase de la carte, fabriquée ici (noms nettoyés).
    pub text: String,
    /// L'outil, ses arguments **sans** `confirmation_id`, et ce qu'on en a déduit.
    pub key: String,
    pub reasons: Vec<Reason>,
    /// Les automatisations à montrer à la personne (noms nettoyés).
    pub rules: Vec<String>,
    /// Dire à l'agent le pourquoi et les règles (le mode : il le connaît déjà, il a « occupation »).
    pub tell_agent: bool,
}

impl Plan {
    /// L'empreinte : la clé et les motifs, pour qu'une autorisation ne serve qu'à la même situation.
    fn fingerprint(&self) -> String {
        let words: Vec<&str> = self.reasons.iter().map(|r| r.word()).collect();
        format!("{}|{}", self.key, words.join(","))
    }
}

/// Ce que l'outil fait ensuite.
pub(super) enum Gate {
    /// Exécuter.
    Go,
    /// Ne rien exécuter : répondre ceci (la demande attend une personne).
    Wait(Value),
}

/// `confirmation_id`, s'il est donné : un texte de 32 chiffres hexadécimaux, sinon refusé.
pub(super) fn confirmation_arg(
    arguments: &Map<String, Value>,
) -> Result<Option<String>, ToolError> {
    match arguments.get("confirmation_id") {
        None => Ok(None),
        Some(Value::String(id)) if confirmations::is_valid_id(id) => Ok(Some(id.clone())),
        Some(_) => Err(ToolError::invalid(
            "confirmation_id : l'identifiant reçu avec confirmation_required (32 chiffres hexadécimaux).",
        )),
    }
}

pub(super) fn id_property() -> Value {
    json!({"type": "string", "description": "L'identifiant reçu avec confirmation_required, une fois la personne d'accord : rappelez l'outil avec les mêmes arguments."})
}

fn ticket_json(ctx: &Ctx<'_>, id: &str, state: State, expires: f64, plan: Option<&Plan>) -> Value {
    let who = ctx
        .accounts
        .display_name(&ctx.agent.user_id)
        .map(|n| {
            let clean = ctx.hub.push_cleaner();
            name_of(&clean, &n)
        })
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "la personne propriétaire de cet agent".to_string());
    let message = match state {
        State::Approved => "Autorisée. Rappelez l'outil avec les mêmes arguments et ce confirmation_id (une seule fois, avant l'expiration).".to_string(),
        _ => format!("En attente de l'accord de {who} dans l'app Ostal (5 minutes). Rappelez l'outil avec les mêmes arguments et ce confirmation_id, ou lisez ostal_confirmation_status."),
    };
    let mut out = json!({
        "status": "confirmation_required",
        "confirmation_id": id,
        "state": state.word(),
        "expires_at": expires,
        "message": message,
    });
    if let Some(plan) = plan.filter(|p| p.tell_agent) {
        out["reasons"] = json!(plan.reasons.iter().map(|r| r.word()).collect::<Vec<_>>());
        out["rules"] = json!(plan.rules);
    }
    out
}

fn invalid_message(e: &ConsumeError) -> &'static str {
    match e {
        ConsumeError::Unknown => "Demande inconnue.",
        ConsumeError::Pending { .. } => "La personne n'a pas encore répondu.",
        ConsumeError::Refused => "La personne a refusé cette demande.",
        ConsumeError::Expired => "Cette demande a expiré : refaites-la.",
        ConsumeError::Used => "Cette autorisation a déjà servi : refaites la demande.",
        ConsumeError::Changed => "Les arguments ou la situation ne sont plus ceux de la demande autorisée : refaites la demande.",
    }
}

/// Le feu : voir le module.
pub(super) fn gate(
    ctx: &Ctx<'_>,
    plan: &Plan,
    confirmation_id: Option<&str>,
) -> Result<Gate, ToolError> {
    let fingerprint = plan.fingerprint();
    let now = now_unix();
    if let Some(id) = confirmation_id {
        return match ctx
            .confirmations
            .consume_at(&ctx.agent.id, id, &fingerprint, now)
        {
            Ok(()) => Ok(Gate::Go),
            Err(ConsumeError::Pending { expires }) => Ok(Gate::Wait(ticket_json(
                ctx,
                id,
                State::Pending,
                expires,
                Some(plan),
            ))),
            Err(e) => Err(ToolError::new("confirmation_invalid", invalid_message(&e))),
        };
    }
    if plan.reasons.is_empty() {
        return Ok(Gate::Go);
    }
    let draft = Draft {
        agent_id: ctx.agent.id.clone(),
        user_id: ctx.agent.user_id.clone(),
        label: ctx.agent.label.clone(),
        who: ctx.who.clone(),
        tool: plan.tool,
        capability: plan.capability,
        fingerprint,
        text: plan.text.clone(),
        details: plan
            .reasons
            .iter()
            .map(|r| r.phrase().to_string())
            .collect(),
        rules: plan.rules.clone(),
    };
    match ctx.confirmations.request_at(draft, now) {
        Ok(Ticket {
            id,
            state,
            expires,
            is_new,
        }) => {
            if is_new {
                ctx.hub.audit().record(
                    Entry::new(Action::ConfirmationRequested, ctx.who)
                        .target(&ctx.agent.label)
                        .detail(plan.tool)
                        .source(Origin::Agent),
                );
                // Un texte générique : il s'affiche sur l'écran verrouillé.
                let clean = ctx.hub.push_cleaner();
                ctx.hub.push().notify_user(
                    &ctx.agent.user_id,
                    Category::AgentRequests,
                    content::agent_request(&clean, &ctx.agent.label),
                );
            }
            Ok(Gate::Wait(ticket_json(ctx, &id, state, expires, Some(plan))))
        }
        Err(RequestError::TooManyLive) => Err(ToolError::new(
            "rate_limited",
            "Trop de demandes en attente pour cet agent (3 au plus) : attendez la réponse d'une personne.",
        )),
        Err(RequestError::TooManyNew) => Err(ToolError::new(
            "rate_limited",
            "Trop de demandes dans l'heure pour cet agent (10 au plus).",
        )),
        Err(RequestError::TableFull) => Err(ToolError::new(
            "rate_limited",
            "Trop de demandes en attente sur ce serveur.",
        )),
    }
}

pub(super) const STATUS: &str = "ostal_confirmation_status";

pub(super) fn status_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"confirmation_id": id_property()},
        "required": ["confirmation_id"],
        "additionalProperties": false
    })
}

/// L'état d'une demande **de ce jeton** : jamais celle d'un autre (« inconnue »).
pub(super) fn status(ctx: &Ctx<'_>, arguments: &Map<String, Value>) -> Result<Value, ToolError> {
    super::tools::only_keys(arguments, &["confirmation_id"])?;
    let Some(id) = confirmation_arg(arguments)? else {
        return Err(ToolError::invalid("confirmation_id est obligatoire."));
    };
    match ctx.confirmations.status_at(&ctx.agent.id, &id, now_unix()) {
        Some((state, expires, tool)) => Ok(json!({
            "confirmation_id": id,
            "state": state.word(),
            "tool": format!("ostal_{tool}"),
            "expires_at": expires,
        })),
        None => Err(ToolError::new("not_found", "Demande inconnue.")),
    }
}

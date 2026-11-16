use foxy_supervisor::maintenance::{MaintenanceReceipt, MaintenanceSnapshot, ResolutionOutcome};

use super::manifest::{encode, fingerprint};
use super::{ActivationJournal, ActivationResolutionRecord, maintenance_intent};
use crate::core::server::{
    ContentStageControl, RecoveryMetadataEvidence, ServerInstanceId, ServerStore,
};

impl ServerStore {
    pub(in crate::core::server) fn inspect_recovery_metadata(
        &self,
        intent: &MaintenanceSnapshot,
        receipt: Option<&MaintenanceReceipt>,
        pending: bool,
    ) -> RecoveryMetadataEvidence {
        let mut evidence = RecoveryMetadataEvidence {
            intent_fingerprint: intent.fingerprint.clone(),
            transaction_fingerprint: intent.intent.transaction_fingerprint.clone(),
            instance_id: intent.intent.owner.instance_id.clone(),
            pending,
            context_status: "foreign_context",
            journal_status: "not_inspected",
            active_endpoint: "not_inspected",
            resolution_status: "not_inspected",
            receipt_status: if receipt.is_some() {
                "recorded"
            } else {
                "absent"
            },
            catalog_unchanged: None,
            selection_unchanged: None,
            inspection_error: None,
        };
        if intent.intent.context_fingerprint != self.activation_context_fingerprint() {
            return evidence;
        }
        evidence.context_status = "local_context";
        let control = ContentStageControl::default();
        let inspect = (|| {
            let id = ServerInstanceId::try_from(intent.intent.owner.instance_id.clone())
                .map_err(|_| "invalid_instance_identity")?;
            let catalog = self.read().map_err(|_| "catalog_unreadable")?.1;
            let selection = if pending {
                self.read_record_state(&id, &control)
                    .map_err(|_| "selection_unreadable")?
                    .1
            } else {
                None
            };
            let expected: ActivationJournal =
                serde_json::from_str(&intent.intent.transaction_evidence)
                    .map_err(|_| "journal_evidence_invalid")?;
            self.validate_activation_journal(&id, &expected)
                .map_err(|_| "journal_evidence_invalid")?;
            if maintenance_intent(&expected).map_err(|_| "journal_evidence_invalid")?
                != intent.intent
                || blake3::hash(&encode(&intent.intent).map_err(|_| "intent_invalid")?)
                    .to_hex()
                    .as_str()
                    != intent.fingerprint
            {
                return Err("intent_evidence_mismatch");
            }
            let journal: Option<(ActivationJournal, String)> = self
                .read_activation_document(&id, "journal.json", &control)
                .map_err(|_| "journal_unreadable")?;
            evidence.journal_status = match &journal {
                None => "absent",
                Some((value, hash))
                    if value == &expected && hash == &intent.intent.transaction_fingerprint =>
                {
                    "matches"
                }
                Some(_) => {
                    if pending {
                        "mismatch"
                    } else {
                        "different_transaction"
                    }
                }
            };
            let active = self
                .read_activation_state(&id, &control)
                .map_err(|_| "active_state_unreadable")?;
            evidence.active_endpoint = if active.as_ref().map(|(value, hash)| (value, hash))
                == expected
                    .previous
                    .as_ref()
                    .zip(expected.previous_fingerprint.as_ref())
            {
                "predecessor"
            } else if active.as_ref().map(|(value, hash)| (value, hash.as_str()))
                == Some((
                    &expected.next,
                    intent.intent.next_state_fingerprint.as_str(),
                ))
            {
                "candidate"
            } else {
                "other_generation"
            };
            let name = format!("resolutions/resolution_{}.json", intent.fingerprint);
            let record: Option<(ActivationResolutionRecord, String)> = self
                .read_activation_document(&id, &name, &control)
                .map_err(|_| "resolution_unreadable")?;
            evidence.resolution_status = if let Some((record, hash)) = &record {
                self.validate_resolution_metadata(&id, record)
                    .map_err(|_| "resolution_invalid")?;
                if &record.intent != intent
                    || record.journal != expected
                    || hash != &fingerprint(record).map_err(|_| "resolution_invalid")?
                {
                    return Err("resolution_evidence_mismatch");
                }
                if let Some(receipt) = receipt {
                    if receipt.evidence_fingerprint != *hash
                        || receipt.resolver != record.resolver
                        || receipt.outcome != record.outcome
                    {
                        return Err("receipt_evidence_mismatch");
                    }
                    evidence.receipt_status = "matches";
                }
                "matches"
            } else {
                "absent"
            };
            if receipt.is_some() && record.is_none() {
                return Err("receipt_evidence_missing");
            }
            if pending && evidence.journal_status == "mismatch" {
                return Err("journal_evidence_mismatch");
            }
            if pending && evidence.active_endpoint == "other_generation" {
                return Err("active_endpoint_ambiguous");
            }
            if pending {
                evidence.catalog_unchanged = Some(
                    catalog.map(|hash| hash.to_hex().to_string()).as_deref()
                        == Some(expected.catalog_fingerprint.as_str()),
                );
                evidence.selection_unchanged =
                    Some(selection.as_deref() == Some(expected.selection_fingerprint.as_str()));
                if let Some((record, _)) = &record {
                    let outcome = if evidence.active_endpoint == "candidate" {
                        ResolutionOutcome::Committed
                    } else {
                        ResolutionOutcome::Aborted
                    };
                    if record.outcome != outcome {
                        return Err("resolution_endpoint_mismatch");
                    }
                }
            }
            if self
                .read_activation_document::<ActivationJournal>(&id, "journal.json", &control)
                .map_err(|_| "journal_unreadable")?
                != journal
                || self
                    .read_activation_state(&id, &control)
                    .map_err(|_| "active_state_unreadable")?
                    != active
                || self
                    .read_activation_document::<ActivationResolutionRecord>(&id, &name, &control)
                    .map_err(|_| "resolution_unreadable")?
                    != record
                || self.read().map_err(|_| "catalog_unreadable")?.1 != catalog
                || (pending
                    && self
                        .read_record_state(&id, &control)
                        .map_err(|_| "selection_unreadable")?
                        .1
                        != selection)
            {
                return Err("local_metadata_changed_during_inspection");
            }
            Ok(())
        })();
        if let Err(error) = inspect {
            evidence.inspection_error = Some(error);
        }
        evidence
    }
}

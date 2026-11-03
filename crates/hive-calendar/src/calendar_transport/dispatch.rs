//! Dormant coordination seam, NOT a distributed lease. Local CAS, nonces and a
//! fresh view cannot fence a paused cross-host worker. There is no expiry takeover.
//! Production minting stays unavailable until a reviewed remote authority factory
//! supplies actual coordination. Test factories can only mint loopback authority.
use super::{
    Binding, DispatchPermit, ImmutableEndpointOrigin, OperationPhase, PreparedFrozenIntent, Result,
    TransportError,
};

struct Basis {
    binding: Binding,
    logical_id: String,
    operation_id: String,
    intent_digest: String,
    ledger_head: String,
    path: String,
    blob: String,
    context_digest: String,
    owner_nonce: String,
    origin: ImmutableEndpointOrigin,
}

pub(super) struct RemoteDispatchAuthority {
    basis: Basis,
    authority: String,
}

pub(super) struct FreshAuthoritativeView {
    basis: Basis,
}

impl RemoteDispatchAuthority {
    pub(super) fn from_fresh(
        receipt: super::coordinator::FreshCasReceipt,
        invocation: super::coordinator::Invocation,
        observed: &super::coordinator::AuthoritativeSnapshot,
        latest: super::coordinator::AuthoritativeSnapshot,
    ) -> anyhow::Result<super::coordinator::InvocationOutcome> {
        receipt.mint(invocation, observed, latest)
    }

    pub(super) fn mint(
        self,
        view: FreshAuthoritativeView,
        intent: &PreparedFrozenIntent,
    ) -> Result<DispatchPermit> {
        let a = self.basis;
        let b = view.basis;
        if a.binding != b.binding
            || a.logical_id != b.logical_id
            || a.operation_id != b.operation_id
            || a.intent_digest != b.intent_digest
            || a.ledger_head != b.ledger_head
            || a.path != b.path
            || a.blob != b.blob
            || a.context_digest != b.context_digest
            || a.owner_nonce != b.owner_nonce
            || a.origin != b.origin
            || a.binding != intent.binding
            || a.logical_id != intent.logical_id
            || a.operation_id != intent.operation_id
            || a.intent_digest != intent.digest
            || Some(&a.context_digest) != intent.context_digest.as_ref()
            || intent.ledger_basis.as_ref()
                != Some(&(a.ledger_head.clone(), a.path.clone(), a.blob.clone()))
            || intent.phase != OperationPhase::Prepared
            || [
                &a.ledger_head,
                &a.path,
                &a.blob,
                &a.context_digest,
                &a.owner_nonce,
                &self.authority,
            ]
            .iter()
            .any(|s| s.is_empty())
        {
            return Err(TransportError::Invalid);
        }
        Ok(DispatchPermit {
            binding: a.binding,
            logical_id: a.logical_id,
            operation_id: a.operation_id,
            digest: a.intent_digest,
            kind: intent.kind,
            origin: a.origin,
            context_digest: Some(a.context_digest),
            ledger_head: Some(a.ledger_head),
            owner_nonce: Some(a.owner_nonce),
            authority: Some(self.authority),
        })
    }
}

#[cfg(test)]
fn fixture_basis(intent: &PreparedFrozenIntent, port: u16) -> Basis {
    Basis {
        binding: intent.binding.clone(),
        logical_id: intent.logical_id.clone(),
        operation_id: intent.operation_id.clone(),
        intent_digest: intent.digest.clone(),
        ledger_head: "a".repeat(40),
        path: "ledger.json".into(),
        blob: "b".repeat(40),
        context_digest: intent.context_digest.clone().unwrap_or_default(),
        owner_nonce: "fixture-owner".into(),
        origin: ImmutableEndpointOrigin::Loopback(port),
    }
}

#[cfg(test)]
fn fixture_authority(intent: &PreparedFrozenIntent, port: u16) -> RemoteDispatchAuthority {
    RemoteDispatchAuthority {
        basis: fixture_basis(intent, port),
        authority: "loopback-fixture".into(),
    }
}

#[cfg(test)]
fn fixture_view(intent: &PreparedFrozenIntent, port: u16) -> FreshAuthoritativeView {
    FreshAuthoritativeView {
        basis: fixture_basis(intent, port),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_authority_requires_exact_fresh_pinned_prepared_basis() {
        let old = super::super::tests::fixture(false);
        let mut intent = PreparedFrozenIntent::prepare(
            &old,
            &old.binding,
            super::super::tests::LOGICAL,
            super::super::tests::OPERATION,
        )
        .unwrap();
        intent.context_digest = Some(format!("sha256:{}", "c".repeat(64)));
        intent.ledger_basis = Some(("a".repeat(40), "ledger.json".into(), "b".repeat(40)));
        for mutation in 0..10 {
            let authority = fixture_authority(&intent, 12345);
            let mut view = fixture_view(&intent, 12345);
            match mutation {
                0 => view.basis.ledger_head = "d".repeat(40),
                1 => view.basis.owner_nonce.push('!'),
                2 => view.basis.context_digest.push('!'),
                3 => view.basis.origin = ImmutableEndpointOrigin::Loopback(12346),
                4 => view.basis.intent_digest.push('!'),
                5 => view.basis.operation_id.push('!'),
                6 => view.basis.logical_id.push('!'),
                7 => view.basis.binding.calendar_id.push('!'),
                8 => view.basis.blob.push('!'),
                _ => view.basis.path.push('!'),
            }
            assert!(authority.mint(view, &intent).is_err());
        }
        let permit = fixture_authority(&intent, 12345)
            .mint(fixture_view(&intent, 12345), &intent)
            .unwrap();
        assert!(permit.origin == ImmutableEndpointOrigin::Loopback(12345));
        intent.phase = OperationPhase::Uncertain;
        assert!(
            fixture_authority(&intent, 12345)
                .mint(fixture_view(&intent, 12345), &intent)
                .is_err()
        );
    }
}

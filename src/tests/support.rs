use crate::{cost::*, payments::*};

pub fn permit(request: &Instruction, mode: Mode) -> DispatchPermit<String> {
    let amount = Amount {
        unit: Unit {
            code: "fixture-fee".into(),
            scale: 0,
        },
        atoms: 0,
    };
    Plan::new(
        vec![Offer {
            candidate: cfbk::Candidate::new(request.adapter.clone(), [request.operation.clone()]),
            quote: Quote {
                price: Price::Exact(amount.clone()),
                source: "inert fixture".into(),
                valid_until: u64::MAX,
                binding: binding(request, mode),
            },
        }],
        cfbk::Config::new([request.adapter.clone()], [request.operation.clone()]),
        CostOrder::Configured,
        amount,
        1,
        mode,
        0,
    )
    .unwrap()
    .start()
    .begin(0)
    .unwrap()
}

impl<J: Journal> Payments<J> {
    pub(crate) async fn execute(&self, request: &Instruction, now: u64) -> Result<Execution> {
        self.dispatch(request, permit(request, Mode::Production), now)
            .await
    }
    pub(crate) async fn observe(&self, id: &[u8; 32], context: &Binding) -> Result<Execution> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static READ: AtomicU64 = AtomicU64::new(1);
        let mut read_id = [255; 32];
        read_id[..8].copy_from_slice(&READ.fetch_add(1, Ordering::SeqCst).to_be_bytes());
        let request = self.observation(id, context, read_id, u64::MAX).await?;
        self.execute(&request, 1).await
    }
}

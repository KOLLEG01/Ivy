use super::*;
use std::{
    cell::{Cell, RefCell},
    future::{Future, poll_fn},
    task::{Poll, Waker},
};

#[derive(Default)]
struct Gate {
    open: Cell<bool>,
    entered: Cell<usize>,
    waiter: RefCell<Option<Waker>>,
}
impl Gate {
    async fn wait(&self) {
        self.entered.set(self.entered.get() + 1);
        poll_fn(|cx| {
            if self.open.get() {
                Poll::Ready(())
            } else {
                *self.waiter.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await;
    }
    fn release(&self) {
        self.open.set(true);
        if let Some(waiter) = self.waiter.borrow_mut().take() {
            waiter.wake();
        }
    }
}
struct HeldObserver {
    gates: [Gate; 2],
    operations: [Document; 2],
    replays: [Arc<Replay>; 2],
    finished: RefCell<Vec<usize>>,
}
impl TaskWriteSink for HeldObserver {
    async fn written(&self, operation: &Document, pin: &Json, task: &Json) -> IvyResult<()> {
        // The routed Task is intentionally a finite source port; the complete
        // creation Task has the source's original UTF16 fields. Do not select
        // expected context from the Operation supplied by the adapter itself.
        let index = usize::from(task.get("retained").is_some());
        let expected = &self.operations[index];
        assert_eq!(operation.pin, expected.pin);
        assert_eq!(operation.content_hash, expected.content_hash);
        assert_eq!(hash_json(&operation.value)?, hash_json(&expected.value)?);
        assert_eq!(operation.parent_id, expected.parent_id);
        self.gates[index].wait().await;
        self.replays[index].call("taskWritten", [pin.clone(), task.clone()])?;
        self.finished.borrow_mut().push(index);
        Ok(())
    }
}
#[tokio::test(flavor = "current_thread")]
async fn interleaved_publications_keep_original_operation_context_and_saved_step_order() {
    let rows = ["create UTF16 fields", "existing route shares Task callback"].map(|name| {
        data()
            .as_array()
            .unwrap()
            .iter()
            .find(|r| s(r, "name") == &JsString::from(name))
            .unwrap()
    });
    let replays = rows.map(|row| {
        Arc::new(Replay {
            name: s(row, "name").clone(),
            steps: Mutex::new(
                f(row, "boundaries")
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
        })
    });
    let calls = rows.map(|row| &f(row, "calls").as_array().unwrap()[0]);
    let originals = calls.map(|call| document(f(call, "operation")));
    let observer = HeldObserver {
        gates: [Gate::default(), Gate::default()],
        operations: originals.clone(),
        replays: replays.clone(),
        finished: RefCell::default(),
    };
    let operations = [0, 1].map(|index| Operations {
        store: World {
            replay: replays[index].clone(),
            root: f(rows[index], "root").as_js_string().cloned(),
        },
        now: clock,
    });
    let bindings = replays.clone().map(Bindings);
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let publishers = [0, 1].map(|index| ObservedPublication {
        operations: &operations[index],
        catalogs: &catalogs,
        checks: &bindings[index],
        routes: &bindings[index],
        observer: &observer,
    });
    let mut first = Box::pin(publishers[0].publish::<Json>(&originals[0], None, None));
    let mut second = Box::pin(publishers[1].publish::<Json>(
        &originals[1],
        None,
        f(calls[1], "normalized").as_js_string(),
    ));
    poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        assert!(second.as_mut().poll(cx).is_pending());
        assert_eq!(observer.gates[0].entered.get(), 1);
        assert_eq!(observer.gates[1].entered.get(), 1);
        Poll::Ready(())
    })
    .await;
    observer.gates[1].release();
    let second = second.await;
    assert_eq!(*observer.finished.borrow(), [1]);
    poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    observer.gates[0].release();
    let first = first.await;
    for (index, result) in [first, second].into_iter().enumerate() {
        let actual = Json::object([("outcomes", Json::Array(vec![outcome(result)]))]);
        assert_eq!(
            hash_json(&actual).unwrap(),
            s(rows[index], "expectedHash").to_utf8().unwrap()
        );
        assert!(
            replays[index].steps.lock().unwrap().is_empty(),
            "Unused original effects"
        );
    }
    assert_eq!(*observer.finished.borrow(), [1, 0, 0]);
}

use super::*;

#[tokio::test(flavor = "current_thread")]
async fn retained_resume_dispatches_only_for_an_active_or_cancelled_known_turn() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    for (known_turn, active, cancelled, permitted) in [
        (true, true, false, true),
        (true, true, true, true),
        (true, false, true, true),
        (true, false, false, false),
        (false, true, true, false),
        (false, false, true, false),
    ] {
        let group = data()
            .as_array()
            .unwrap()
            .iter()
            .find(|g| {
                g.required("base")
                    .unwrap()
                    .required("status")
                    .unwrap()
                    .get("nativeVersion")
                    == Some(&"0.159.2".into())
            })
            .unwrap();
        let base = group.required("base").unwrap();
        let input = Json::object([
            ("slot", "thread".into()),
            (
                "operations",
                Json::Array(vec!["absent".into(), "accepted".into()]),
            ),
        ]);
        let db = Connection::open_in_memory().unwrap();
        local_journal::initialize(&db).unwrap();
        let allowed = Rc::new(RefCell::new(HashSet::new()));
        let state = State {
            log: Arc::default(),
            fault: RefCell::new(Json::Null),
            scopes: RefCell::default(),
            identities: Cell::new(0),
            named: Cell::new(0),
            operation: None,
        };
        let client = Arc::new(Client::new(&input, base, state.log.clone(), &catalogs));
        let next = Cell::new(400);
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: Policy {
                allowed: allowed.clone(),
            },
            limits: HistoryLimits::default(),
            now: || TIME.to_owned(),
            new_id: || {
                let n = next.get();
                next.set(n + 1);
                id(n)
            },
            cache: None,
        };
        let trace = Trace {
            inner: &store,
            state: &state,
        };
        let mut run = base.required("run").unwrap().clone();
        run.insert(
            "turnId",
            if known_turn {
                "turn".into()
            } else {
                Json::Null
            },
        );
        run.insert(
            "cancellation",
            if cancelled {
                Json::object([("reason", "saved cancellation".into())])
            } else {
                Json::Null
            },
        );
        let mut calls = run.required("calls").unwrap().clone();
        calls.insert("thread", pin(300, 1.0));
        run.insert("calls", calls);
        let mut task = base.required("task").unwrap().clone();
        if !active {
            task.insert("claim", Json::Null);
        }
        let call = base.required("calls").unwrap().required("thread").unwrap();
        assert_eq!(
            call.required("request").unwrap().get("method"),
            Some(&"thread/resume".into())
        );
        seed(
            &store.journal,
            &allowed,
            "task-board/native-call",
            300,
            1.0,
            call,
        );
        for revision in [1.0, 2.0] {
            seed(
                &store.journal,
                &allowed,
                "task-board/run",
                200,
                revision,
                &run,
            );
            seed(
                &store.journal,
                &allowed,
                "task-board/task",
                100,
                revision,
                &task,
            );
        }
        let runtime = Runtime {
            state: &state,
            tools: Arc::new(ToolClients::new(client.clone())),
            principal: "workflow-owner".into(),
            owners: Cell::new(0),
            input: &input,
            changed: |_| Ok(()),
        };
        let driver = NativeDriver {
            store: &trace,
            runtime: &runtime,
            prompts: &NoPrompts,
            catalogs: &catalogs,
        };
        driver.advance(&id(200), NativeSlot::Thread).await.unwrap();
        assert_eq!(
            *client.invokes.lock().unwrap(),
            u64::from(permitted),
            "known={known_turn}, active={active}, cancelled={cancelled}"
        );
        assert_eq!(
            state.identities.get(),
            0,
            "Recovery must retain its operation identity"
        );
        let log = state.log.lock().unwrap();
        let calls: Vec<_> = log
            .iter()
            .filter_map(|v| {
                let a = v.as_array()?;
                (a.first() == Some(&"rpc".into()) && a.get(1) == Some(&"tools.call".into()))
                    .then(|| &a[2])
            })
            .collect();
        assert_eq!(
            calls
                .iter()
                .filter(|v| v.get("qualifiedName") == Some(&"agent.prevent".into()))
                .count(),
            usize::from(!permitted)
        );
        for call in calls
            .iter()
            .filter(|v| v.get("qualifiedName") == Some(&"agent.invoke".into()))
        {
            let args = call.required("arguments").unwrap();
            assert_eq!(args.get("method"), Some(&"thread/resume".into()));
            assert_eq!(args.get("operationId"), Some(&"original".into()));
            assert_eq!(
                hash_json(args.required("params").unwrap()).unwrap(),
                hash_json(
                    base.required("calls")
                        .unwrap()
                        .required("thread")
                        .unwrap()
                        .required("request")
                        .unwrap()
                        .required("params")
                        .unwrap()
                        .required("params")
                        .unwrap()
                )
                .unwrap()
            );
        }
    }
}

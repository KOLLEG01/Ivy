use ivy_contracts::IvyJson;

pub(super) fn turns(values: &[IvyJson]) -> IvyJson {
    IvyJson::object([
        (
            "data",
            IvyJson::Array(
                values
                    .iter()
                    .map(|turn| {
                        let mut turn = turn.clone();
                        turn.insert("items", IvyJson::Array(vec![]));
                        turn.insert("itemsView", "notLoaded".into());
                        turn
                    })
                    .collect(),
            ),
        ),
        ("nextCursor", IvyJson::Null),
    ])
}
pub(super) fn items(turn: &IvyJson) -> IvyJson {
    IvyJson::object([
        (
            "data",
            IvyJson::Array(
                turn.required("items")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let mut item = item.clone();
                        if item.get("id").is_none() {
                            item.insert("id", format!("fixture-item-{index}").into());
                        }
                        IvyJson::object([
                            ("turnId", turn.required("id").unwrap().clone()),
                            ("item", item),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("nextCursor", IvyJson::Null),
    ])
}

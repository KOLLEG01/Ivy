use ivy_contracts::{IvyError, IvyJson, IvyResult, json::parse};

/// Complete published definitions in taskBoardContracts() return order.
/// Historical versions retain their original schemas, retention and specs.
pub fn task_board_contracts() -> IvyResult<Vec<IvyJson>> {
    let definitions = include_str!("contracts.json");
    parse(definitions, definitions.len())?
        .into_array()
        .ok_or_else(IvyError::internal)
}

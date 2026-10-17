use futures_util::{StreamExt, stream::FuturesUnordered};
use ivy_contracts::{IvyError, IvyResult};
use std::future::Future;

/// Stop admission at first error and drain all started work before returning it.
pub async fn map_concurrent<T, R, F, Fut>(
    items: Vec<T>,
    concurrency: usize,
    action: F,
) -> IvyResult<Vec<R>>
where
    F: Fn(T, usize) -> Fut,
    Fut: Future<Output = IvyResult<R>>,
{
    if !(1..=8).contains(&concurrency) {
        return Err(IvyError::new(
            "invalid_arguments",
            "Owned concurrent work requires a bounded worker count.",
        ));
    }
    let mut items = items.into_iter().enumerate();
    let mut results = Vec::new();
    let mut running = FuturesUnordered::new();
    let mut failure = None;
    let run = |index, item| {
        let action = &action;
        async move { (index, action(item, index).await) }
    };
    for _ in 0..concurrency {
        if let Some((index, item)) = items.next() {
            running.push(run(index, item));
        }
    }
    while let Some((index, result)) = running.next().await {
        match result {
            Ok(value) => results.push((index, value)),
            Err(error) => {
                if failure.is_none() {
                    failure = Some(error);
                }
            }
        }
        if failure.is_none() {
            if let Some((index, item)) = items.next() {
                running.push(run(index, item));
            }
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }
    results.sort_by_key(|(i, _)| *i);
    Ok(results.into_iter().map(|(_, v)| v).collect())
}

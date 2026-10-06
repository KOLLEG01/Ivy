import { expect } from "@playwright/test";

/** Chooses a shared OptionSelect entry by value, or by visible label, like Playwright selectOption. */
export async function choose(trigger, value) {
  await trigger.click();
  const list = trigger.page().getByRole("listbox");
  await expect(list).toBeVisible();
  const byValue = list.locator(
    `[role="option"]:has([data-value="${value.replace(/["\\]/g, "\\$&")}"])`,
  );
  // Options render after the list opens; wait for either a value or a visible label match.
  await byValue
    .or(list.getByRole("option", { name: value, exact: true }))
    .first()
    .click();
  await expect(list).toHaveCount(0);
}

/** Lists the visible labels an OptionSelect offers, then closes it unchanged. */
export async function options(trigger) {
  await trigger.click();
  const list = trigger.page().getByRole("listbox");
  await expect(list).toBeVisible();
  const labels = (await list.getByRole("option").allTextContents()).map(
    (label) => label.trim(),
  );
  await trigger.page().keyboard.press("Escape");
  await expect(list).toHaveCount(0);
  return labels;
}

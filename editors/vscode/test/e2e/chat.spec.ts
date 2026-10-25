import { expect, test, webviewWith } from './fixtures';
import { filterCommandPalette } from './helpers';

// The Chat view against the real stack (BRD AC8): a question is a `chat` run of the minted-mode
// runner on the echo harness, answered as the signed-in person and pushed live over /ws. The echo
// answers "echo: <question>", so a question that names a record gives an answer that cites it.
test.use({ suite: 'chat' });
test.describe.configure({ mode: 'serial' });

test('a new chat is answered without a reload; its link opens the record, its run shows how', async ({
  stack,
}) => {
  const { page } = stack;
  await filterCommandPalette(page, 'Escurel: New chat', 'New chat');
  await page.keyboard.press('Enter');
  const chat = await webviewWith(page, 'escurel-chat');
  await expect(chat.locator('escurel-chat')).toContainText('New chat');

  const box = chat.getByRole('textbox', { name: 'Ask a question' });
  await box.fill('What do we know about [[customer-order::order-4500123]]?');
  await box.press('Enter');

  // The question shows at once; the answer arrives over the socket, with no reload.
  await expect(chat.getByRole('article', { name: 'Your question' })).toContainText(
    'What do we know about',
  );
  const answer = chat.getByRole('article', { name: 'Answer' });
  await expect(answer).toContainText('echo: What do we know about', { timeout: 90_000 });
  // The new conversation is in Recent, titled after its first question.
  await expect(chat.locator('details.recent')).toContainText('What do we know about');
  await stack.shot('chat-01-answered');

  // The cited record opens as a page.
  await answer.getByRole('button', { name: /order-4500123/ }).click();
  await expect(page.locator('.tab .label-name', { hasText: 'order-4500123' })).toBeVisible({
    timeout: 30_000,
  });

  // "Show how I got this" opens the run that answered.
  const chatAgain = await webviewWith(page, 'escurel-chat');
  await chatAgain.getByRole('button', { name: 'Show how I got this' }).click();
  await expect(page.locator('.tab .label-name', { hasText: /^Run / })).toBeVisible({
    timeout: 30_000,
  });
  await stack.shot('chat-02-run');
  // The window logs VS Code's own noise (other extensions); the chat view must add none.
  expect(stack.errors.filter((e) => /escurel-chat|chatView/.test(e))).toEqual([]);
});

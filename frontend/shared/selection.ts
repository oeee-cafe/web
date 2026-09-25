/**
 * The runtime half of selection.css: a selection that appears on a drawing
 * page anyway is cleared the moment it does.
 *
 * The stylesheet refuses selection for the whole page, and that is the
 * answer that should hold. It has not always: iOS has raised the loupe from
 * a long press inside `-webkit-user-select: none` before, and a press with
 * a stylus is the press most likely to be held. A selection here is a mode,
 * not a mark -- while one is up the browser spends every press dismissing
 * it, on a touch screen on the handles and the callout it drew, and nothing
 * on a page of buttons and canvas ever clears it: the canvas takes no
 * stroke and the toolbox no press until a reload. Clearing it on
 * `selectionchange` ends the mode before the next press has to.
 *
 * Fields you type in keep theirs, the painter's text tool included: those
 * are the same exception the stylesheet makes.
 */
export function refuseSelection(doc: Document = document): () => void {
  const onSelectionChange = () => {
    const selection = doc.getSelection();
    if (!selection || selection.isCollapsed) return;
    if (isEditable(selection.anchorNode) || isEditable(selection.focusNode)) return;
    selection.removeAllRanges();
  };
  doc.addEventListener("selectionchange", onSelectionChange);
  return () => doc.removeEventListener("selectionchange", onSelectionChange);
}

function isEditable(node: Node | null): boolean {
  const element = node instanceof Element ? node : node?.parentElement;
  if (!element) return false;
  return element.closest("input, textarea, [contenteditable]") !== null;
}

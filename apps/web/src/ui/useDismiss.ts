import { useEffect, type RefObject } from 'react';

/**
 * Close a popover on Escape or on a pointer press outside it.
 *
 * `pointerdown` rather than `click`: a press that starts inside the menu and ends outside
 * it — a drag over a scrollbar, or a text selection — is not a dismissal, and `click`
 * fires on the common ancestor and closes the menu anyway. Listening in the capture phase
 * means a handler inside the menu cannot swallow the event first.
 */
export function useDismiss(
  ref: RefObject<HTMLElement | null>,
  open: boolean,
  onDismiss: () => void,
): void {
  useEffect(() => {
    if (!open) {
      return;
    }

    function onPointerDown(event: PointerEvent) {
      const target = event.target;
      if (target instanceof Node && ref.current?.contains(target) === true) {
        return;
      }
      onDismiss();
    }

    function onKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') {
        onDismiss();
      }
    }

    document.addEventListener('pointerdown', onPointerDown, true);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown, true);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [ref, open, onDismiss]);
}

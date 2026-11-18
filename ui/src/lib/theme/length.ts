// Positioning and Svelte transitions use CSS pixels, while their tokens scale with the root.
export function tokenPixels(element: Element, name: string): number {
  const value = getComputedStyle(element).getPropertyValue(name).trim();
  const length = Number.parseFloat(value) || 0;
  if (!value.endsWith('rem')) return length;
  return (
    length *
    Number.parseFloat(getComputedStyle(document.documentElement).fontSize)
  );
}

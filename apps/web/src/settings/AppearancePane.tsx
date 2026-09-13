import { THEMES, THEME_NAMES, type ThemeName } from '../theme/themes';
import { useTheme } from '../theme/useTheme';
import { SettingRow } from '../ui/SettingRow';
import { SettingsCard, SettingsHeader } from './layout';

/**
 * Settings › Appearance — the pane that drives the theme and the accent.
 *
 * Both are live: picking Graphite or a new accent rewrites the twelve custom properties on
 * `<html>` in the same frame, so the settings screen you are looking at repaints along with
 * the titlebar, the rail and any popover behind it. Nothing here re-renders the tree by
 * hand; the change is a CSS variable write and the whole window follows.
 *
 * Swatches preview themselves in the colours they will apply, which is the one place a
 * component is allowed to paint from the theme *table* rather than the current token — the
 * whole point of the control is to show what you have not switched to yet.
 */
export function AppearancePane() {
  const { theme, accent, presets, setTheme, setAccent } = useTheme();

  return (
    <>
      <SettingsHeader
        title="Appearance"
        description="Theme and accent apply immediately, across the whole window."
      />

      <SettingsCard>
        <SettingRow
          divider={false}
          label="Theme"
          description="Ember is a cool blue-black. Graphite is a neutral warm grey."
          control={
            <div role="radiogroup" aria-label="Theme" className="flex flex-none gap-2">
              {THEME_NAMES.map((name) => (
                <ThemeSwatch
                  key={name}
                  name={name}
                  selected={name === theme}
                  onSelect={() => setTheme(name)}
                />
              ))}
            </div>
          }
        />

        <SettingRow
          label="Accent"
          description="One hue drives the identity colour and the two translucent variants derived from it. Agent status colours stay independent."
          control={
            <div className="flex flex-none items-center gap-2">
              <div role="radiogroup" aria-label="Accent preset" className="flex gap-2">
                {presets.map((preset) => (
                  <button
                    key={preset.value}
                    type="button"
                    role="radio"
                    aria-checked={preset.value === accent}
                    aria-label={preset.label}
                    title={preset.label}
                    onClick={() => setAccent(preset.value)}
                    style={{ background: preset.value }}
                    className={`size-6 cursor-pointer rounded-full focus-visible:shadow-focus focus-visible:outline-none ${
                      preset.value === accent
                        ? 'ring-fg ring-2 ring-offset-2 ring-offset-[var(--color-bg0)]'
                        : 'border-line border'
                    }`}
                  />
                ))}
              </div>
              <label className="border-line2 bg-bg2 flex items-center gap-2 rounded-control border px-2 py-1 focus-within:shadow-focus">
                <span className="sr-only">Custom accent</span>
                <input
                  type="color"
                  value={accent}
                  onChange={(event) => setAccent(event.target.value)}
                  className="size-5 cursor-pointer border-0 bg-transparent p-0"
                />
                <span className="text-fg2 font-mono text-[11px]">{accent}</span>
              </label>
            </div>
          }
        />
      </SettingsCard>
    </>
  );
}

/**
 * A theme swatch: three stacked bands of the theme's own `bg0`, `bg2` and `fg`, so the
 * choice reads as the palette it is rather than as a word.
 */
function ThemeSwatch({
  name,
  selected,
  onSelect,
}: {
  readonly name: ThemeName;
  readonly selected: boolean;
  readonly onSelect: () => void;
}) {
  const palette = THEMES[name];

  return (
    <button
      type="button"
      role="radio"
      aria-checked={selected}
      onClick={onSelect}
      className={`flex cursor-pointer items-center gap-2.5 rounded-control px-3 py-2 focus-visible:shadow-focus focus-visible:outline-none ${
        selected ? 'border-acc bg-acc14 text-fg border' : 'border-line2 text-fg2 border'
      }`}
    >
      <span
        aria-hidden="true"
        className="border-line flex size-5 flex-none overflow-hidden rounded-full border"
      >
        <span className="flex-1" style={{ background: palette.bg0 }} />
        <span className="flex-1" style={{ background: palette.bg2 }} />
        <span className="flex-1" style={{ background: palette.fg }} />
      </span>
      {name}
    </button>
  );
}

// The frontend side of the theme module (SPEC.md §3, §6). Import it once, before
// any component, so every component can use var(--token): the base tokens copied
// from the reference's :root, then the supplemental reference values.
//
// Fonts are bundled into the app rather than fetched from a CDN. Inter and
// JetBrains Mono are the fallbacks in --sans and --mono after SF Pro and SF Mono,
// which Windows doesn't ship. Only upright faces are bundled, because Velocity
// uses no italics; these are the weights the reference UI uses.
import '@fontsource/inter/400.css';
import '@fontsource/inter/500.css';
import '@fontsource/inter/600.css';
import '@fontsource/inter/700.css';
import '@fontsource/jetbrains-mono/400.css';
import '@fontsource/jetbrains-mono/500.css';
import '@fontsource/jetbrains-mono/600.css';
import './tokens.css';
import './reference.css';

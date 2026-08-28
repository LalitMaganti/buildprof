// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

const WORDMARK = `
<svg xmlns="http://www.w3.org/2000/svg" width="184" height="40" viewBox="0 0 184 40">
  <g fill="none" stroke="#ffffff" stroke-width="2.5" stroke-linecap="round">
    <path d="M3 27h8V13h8v18h8V8h8v19h8"/>
    <path d="M3 33h40" opacity=".45"/>
  </g>
  <text x="53" y="28" fill="#ffffff" font-family="Inter,Roboto,sans-serif" font-size="23" font-weight="500" letter-spacing="-.5">buildprof</text>
</svg>`;

const HERO = `
<svg xmlns="http://www.w3.org/2000/svg" width="260" height="64" viewBox="0 0 260 64">
  <g fill="none" stroke="#1f6feb" stroke-width="4" stroke-linecap="round">
    <path d="M4 43h13V21h13v29h13V13h13v30h13"/>
    <path d="M4 53h65" opacity=".35"/>
  </g>
  <text x="84" y="45" fill="#202124" font-family="Inter,Roboto,sans-serif" font-size="34" font-weight="550" letter-spacing="-1">buildprof</text>
</svg>`;

function svgDataUrl(svg: string): string {
  return `data:image/svg+xml,${encodeURIComponent(svg.trim())}`;
}

export const BUILDPROF_WORDMARK = svgDataUrl(WORDMARK);
export const BUILDPROF_HERO = svgDataUrl(HERO);

// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

import './buildprof_home_page.scss';
import m from 'mithril';
import type {HomePageAttrs} from './embedder';
import {BUILDPROF_HERO} from './buildprof_brand';
import type {App} from '../../public/app';
import {VERSION} from '../../virtual/version';

const RELEASES_URL = 'https://github.com/LalitMaganti/buildprof/releases';
const INSTALL_COMMAND =
  "curl --proto '=https' --tlsv1.2 -LsSf " +
  `${RELEASES_URL}/latest/download/buildprof-installer.sh | sh`;
const OTHER_INSTALLS = [
  'brew install lalitmaganti/tap/buildprof',
  'mise use -g buildprof',
  'cargo install buildprof --locked',
];
const RECORD_COMMAND = 'buildprof -- make -j8';

// Recordings served from the production site next to the UI; keep in step
// with EXAMPLES in src/args.rs. Absolute so self-hosted UIs still find them.
const EXAMPLES_URL = 'https://buildprof.lalitm.com/examples';
interface Example {
  readonly name: string;
  readonly file: string;
  readonly description: string;
}
const EXAMPLES: ReadonlyArray<Example> = [
  {
    name: 'ripgrep',
    file: 'ripgrep-release-clean.buildprof',
    description: 'A clean `cargo build --release` of ripgrep',
  },
];

function chooseAndOpenTrace(app: App): void {
  const input = document.createElement('input');
  input.type = 'file';
  input.accept = '.buildprof,.pftrace,.perfetto-trace,.trace';
  input.onchange = () => {
    const file = input.files?.[0];
    if (file !== undefined) void app.openTraceFromFile(file);
  };
  input.click();
}

function openExample(app: App, example: Example): void {
  void app.openTraceFromUrl(`${EXAMPLES_URL}/${example.file}`);
}

function commandBlock(command: string) {
  return m('div.bt-home__command', m('span', '$'), m('code', command));
}

export class BuildprofHomePage implements m.ClassComponent<HomePageAttrs> {
  view({attrs}: m.CVnode<HomePageAttrs>) {
    return m(
      'main.bt-home',
      m(
        'header.bt-home__hero',
        m('img.bt-home__logo', {src: BUILDPROF_HERO, alt: 'buildprof'}),
        m('h1', 'See what your build is actually doing.'),
        m(
          'p.bt-home__lede',
          'Record every process and exec, then explore the build on one timeline.',
        ),
      ),
      m(
        'section.bt-home__steps',
        m(
          'article.bt-home__step',
          m('span.bt-home__number', '1'),
          m('h2', 'Install Buildprof'),
          m('p', 'Install the recorder on your Linux build machine.'),
          commandBlock(INSTALL_COMMAND),
          m(
            'p.bt-home__alternatives',
            'Or: ',
            ...OTHER_INSTALLS.flatMap((command, index) => [
              index > 0 ? ' · ' : '',
              m('code', command),
            ]),
          ),
        ),
        m(
          'article.bt-home__step',
          m('span.bt-home__number', '2'),
          m('h2', 'Record a build'),
          m(
            'p',
            'Put Buildprof before any build command. When recording finishes, the trace opens in this UI.',
          ),
          commandBlock(RECORD_COMMAND),
        ),
        m(
          'article.bt-home__step.bt-home__step--open',
          m('span.bt-home__number', '3'),
          m('h2', 'Open an existing build'),
          m(
            'p',
            'Already have a Buildprof recording? Open its .buildprof file here.',
          ),
          m(
            'button.bt-home__open',
            {type: 'button', onclick: () => chooseAndOpenTrace(attrs.app)},
            'Open a recording',
          ),
        ),
      ),
      m(
        'section.bt-home__examples',
        m('h2', 'Or explore an example'),
        m(
          'div.bt-home__example-list',
          EXAMPLES.map((example) =>
            m(
              'button.bt-home__example',
              {
                type: 'button',
                onclick: () => openExample(attrs.app, example),
              },
              m('strong', example.name),
              m('span', example.description),
            ),
          ),
        ),
      ),
      m(
        'section.bt-home__shortcuts',
        m('h2', 'Keyboard shortcuts'),
        m(
          'div.bt-home__shortcut-list',
          m(
            'div.bt-home__shortcut',
            m('span', 'Navigate timeline'),
            m(
              'span.bt-home__keys',
              m('kbd', 'W'),
              m('kbd', 'A'),
              m('kbd', 'S'),
              m('kbd', 'D'),
            ),
          ),
        ),
      ),
      m(
        'aside.bt-home__privacy',
        m(
          'span',
          m('strong', 'Private by default. '),
          'All data is recorded and processed on your machine. Nothing is uploaded without your explicit consent.',
        ),
      ),
      m(
        'footer.bt-home__version',
        m(
          'a',
          {href: RELEASES_URL, target: '_blank', rel: 'noopener'},
          `Buildprof UI ${VERSION}`,
        ),
      ),
    );
  }
}

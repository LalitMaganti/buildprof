// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

import m from 'mithril';
import {showModal} from '../../widgets/modal';

export function showBuildprofHelp(): void {
  void showModal({
    title: 'Navigate a build trace',
    icon: 'help_outline',
    content: m(
      'div',
      m('p', 'Move around the timeline with the keyboard or mouse:'),
      m(
        'ul',
        m('li', m('strong', 'W / S'), ' — zoom in / out'),
        m('li', m('strong', 'A / D'), ' — pan left / right'),
        m('li', m('strong', 'Ctrl / ⌘ + scroll'), ' — zoom at the pointer'),
        m('li', m('strong', 'Shift + drag'), ' — pan left / right'),
        m('li', m('strong', 'Click an event'), ' — inspect its details below'),
        m('li', m('strong', 'Click + drag'), ' — select a time range'),
      ),
    ),
    buttons: [{text: 'Got it', primary: true}],
  });
}

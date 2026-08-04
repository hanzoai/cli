/* The Hanzo terminal client.
 *
 * ttyd's own page is a fine terminal and the wrong surface for this product: it
 * cannot be themed to match the console that frames it, it has no touch
 * affordances, and — because it is somebody else's document inside a
 * cross-origin frame — it swallows every keystroke the workspace would like to
 * hear. Owning the page answers all three at once. ttyd stays exactly as it is;
 * only `--index` changes.
 *
 * THE PROTOCOL is ttyd's and is small. The socket carries a one-byte opcode:
 *   send  '0'+utf8   input          '1'+json{columns,rows}  resize
 *   recv  '0'+bytes  output         '1'+title               '2'+json prefs
 * The first frame the client sends is a JSON auth object; ttyd waits for it
 * before spawning the command, which is why the shell does not exist until a
 * browser actually connects.
 */
(function () {
  'use strict';

  var TERM = new window.Terminal({
    // Monochrome, matching hanzo.app rather than xterm's defaults. The terminal
    // is the deepest surface in the product, so it is true black.
    theme: {
      background: '#000000',
      foreground: '#e6e6e6',
      cursor: '#e6e6e6',
      cursorAccent: '#000000',
      selectionBackground: 'rgba(255,255,255,0.18)',
      black: '#000000', brightBlack: '#5c5c5c',
      red: '#f87171', brightRed: '#fca5a5',
      green: '#34d399', brightGreen: '#6ee7b7',
      yellow: '#fbbf24', brightYellow: '#fcd34d',
      blue: '#60a5fa', brightBlue: '#93c5fd',
      magenta: '#c084fc', brightMagenta: '#d8b4fe',
      cyan: '#22d3ee', brightCyan: '#67e8f9',
      white: '#e6e6e6', brightWhite: '#ffffff',
    },
    fontFamily:
      'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace',
    fontSize: 13,
    lineHeight: 1.2,
    cursorBlink: true,
    cursorStyle: 'bar',
    // A terminal you cannot scroll back through is a log you cannot read.
    scrollback: 10000,
    allowProposedApi: true,
    macOptionIsMeta: true,
  });

  var FIT = new window.FitAddon.FitAddon();
  TERM.loadAddon(FIT);
  TERM.open(document.getElementById('term'));

  // ---- the socket ---------------------------------------------------------
  var proto = location.protocol === 'https:' ? 'wss://' : 'ws://';
  var url = proto + location.host + location.pathname.replace(/\/$/, '') + '/ws' + location.search;
  var ws = new WebSocket(url, ['tty']);
  ws.binaryType = 'arraybuffer';

  var enc = new TextEncoder();
  var dec = new TextDecoder();

  function send(op, payload) {
    if (ws.readyState !== WebSocket.OPEN) return;
    var body = enc.encode(payload);
    var frame = new Uint8Array(body.length + 1);
    frame[0] = op.charCodeAt(0);
    frame.set(body, 1);
    ws.send(frame);
  }

  function fit() {
    try {
      FIT.fit();
      send('1', JSON.stringify({ columns: TERM.cols, rows: TERM.rows }));
    } catch (e) {
      /* a zero-size container during layout; the next resize settles it */
    }
  }

  ws.onopen = function () {
    // ttyd spawns the command only after this frame.
    ws.send(JSON.stringify({ AuthToken: '', columns: TERM.cols, rows: TERM.rows }));
    fit();
    TERM.focus();
  };

  ws.onmessage = function (e) {
    var data = typeof e.data === 'string' ? enc.encode(e.data) : new Uint8Array(e.data);
    if (!data.length) return;
    var op = String.fromCharCode(data[0]);
    var rest = data.subarray(1);
    if (op === '0') TERM.write(rest);
    else if (op === '1') document.title = dec.decode(rest);
  };

  ws.onclose = function () {
    TERM.write('\r\n\x1b[2m— disconnected —\x1b[0m\r\n');
  };

  TERM.onData(function (d) { send('0', d); });
  TERM.onBinary(function (d) { send('0', d); });

  window.addEventListener('resize', fit);
  // The pane is resized by the parent dragging a divider, which fires no window
  // resize inside the frame. The container is what actually changes.
  if (window.ResizeObserver) new ResizeObserver(fit).observe(document.getElementById('term'));

  // ---- talking to the workspace that frames us ----------------------------
  //
  // A cross-origin frame receives every key and gives the parent none, so the
  // workspace's shortcuts can only exist if this page forwards them. It forwards
  // the ⌃⌥ set and NOTHING else: Ctrl belongs to the shell, Alt is readline's
  // Meta, ⌘ is the browser's. `⌃⌥` is bound by nothing in bash, zsh, vim or tmux.
  //
  // AltGr reports as ctrl+alt on European layouts, so it is excluded explicitly —
  // otherwise AltGr+E (€) would read as a workspace chord.
  var PARENT = '*';
  function post(msg) {
    if (window.parent !== window) window.parent.postMessage(Object.assign({ v: 1 }, msg), PARENT);
  }

  TERM.attachCustomKeyEventHandler(function (e) {
    if (e.type !== 'keydown') return true;
    if (!e.ctrlKey || !e.altKey) return true;
    if (e.getModifierState && e.getModifierState('AltGraph')) return true;
    post({ t: 'chord', key: e.key, shift: e.shiftKey });
    e.preventDefault();
    return false; // consumed by the workspace, never sent to the pty
  });

  document.addEventListener('pointerdown', function () { post({ t: 'focus' }); }, true);
  window.addEventListener('message', function (e) {
    var d = e.data;
    if (!d || d.v !== 1) return;
    if (d.t === 'input' && typeof d.data === 'string') { send('0', d.data); TERM.focus(); }
    if (d.t === 'focus') TERM.focus();
    if (d.t === 'size' && typeof d.px === 'number') { TERM.options.fontSize = d.px; fit(); }
  });
  post({ t: 'hello' });

  // ---- the key row --------------------------------------------------------
  //
  // A soft keyboard has no Esc, no Ctrl, no Tab and no arrows, so on a touch
  // device a terminal without this row is a terminal you can read and not use.
  // Every serious iOS client ships one; this is that row.
  var TOUCH = window.matchMedia('(pointer: coarse)').matches;
  if (TOUCH) {
    var KEYS = [
      ['esc', '\x1b'], ['tab', '\t'], ['ctrl', null], ['/', '/'], ['|', '|'],
      ['-', '-'], ['~', '~'], ['↑', '\x1b[A'], ['↓', '\x1b[B'],
      ['←', '\x1b[D'], ['→', '\x1b[C'], ['^C', '\x03'], ['^D', '\x04'], ['^R', '\x12'],
    ];
    var armed = false;
    var row = document.getElementById('keys');
    row.hidden = false;
    KEYS.forEach(function (k) {
      var b = document.createElement('button');
      b.textContent = k[0];
      b.addEventListener('pointerdown', function (ev) {
        ev.preventDefault();
        if (k[1] === null) {
          // Sticky Ctrl: tap, then tap a letter. The only way to reach ^C on a
          // soft keyboard, and how Blink and Termius both do it.
          armed = !armed;
          b.dataset.armed = armed ? '1' : '';
          return;
        }
        if (armed && k[1].length === 1) {
          var c = k[1].toUpperCase().charCodeAt(0);
          if (c >= 64 && c < 128) send('0', String.fromCharCode(c & 31));
          armed = false;
          delete row.querySelector('[data-armed]')?.dataset.armed;
        } else {
          send('0', k[1]);
        }
        TERM.focus();
      });
      row.appendChild(b);
    });
    // Keep the row above the software keyboard rather than under it.
    if (window.visualViewport) {
      var vv = window.visualViewport;
      var place = function () {
        var inset = window.innerHeight - (vv.height + vv.offsetTop);
        row.style.transform = 'translateY(' + -Math.max(0, inset) + 'px)';
        fit();
      };
      vv.addEventListener('resize', place);
      vv.addEventListener('scroll', place);
      place();
    }
  }
})();

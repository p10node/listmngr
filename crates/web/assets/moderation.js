// Keyboard shortcuts for the held queue. Nothing here is required: every
// decision is a plain form. With the page's own script allowed, a moderator
// can move through the queue and decide without the mouse.
//
//   j / k   focus the next / previous held post
//   a r d h choose accept / reject / discard / keep held for the focused post
//   s       submit the focused post's decision
//   ?       show this list
(function () {
  'use strict';
  var posts = Array.prototype.slice.call(document.querySelectorAll('article[data-held]'));
  if (!posts.length) { return; }
  posts.forEach(function (post) { post.tabIndex = -1; });
  var help = document.getElementById('shortcuts-help');
  if (help) { help.hidden = false; }
  function current() {
    var active = document.activeElement;
    var inside = active && active.closest ? active.closest('article[data-held]') : null;
    return inside || posts[0];
  }
  function move(step) {
    var index = posts.indexOf(current());
    var next = posts[Math.min(posts.length - 1, Math.max(0, index + step))];
    next.focus();
    next.scrollIntoView({ block: 'nearest' });
  }
  function choose(value) {
    var select = current().querySelector('select[name="action"]');
    if (select) { select.value = value; select.focus(); }
  }
  document.addEventListener('keydown', function (event) {
    if (event.altKey || event.ctrlKey || event.metaKey) { return; }
    var target = event.target;
    if (target && target.closest && target.closest('input, textarea, select')) { return; }
    switch (event.key) {
      case 'j': move(1); break;
      case 'k': move(-1); break;
      case 'a': choose('accept'); break;
      case 'r': choose('reject'); break;
      case 'd': choose('discard'); break;
      case 'h': choose('defer'); break;
      case 's': {
        var form = current().querySelector('form.decision');
        if (form) { form.requestSubmit(); }
        break;
      }
      case '?': if (help) { help.open = !help.open; } break;
      default: return;
    }
    event.preventDefault();
  });
})();

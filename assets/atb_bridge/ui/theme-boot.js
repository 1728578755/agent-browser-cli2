/* 主题引导：在 <head> 里同步执行，尽量避免「先白后黑」的闪屏。
 *
 * MV3 的扩展页面不能用内联脚本，所以单独成一个文件。它只做一件事：
 * 按系统偏好先打 data-resolved；用户在设置里选的固定主题稍后由 ui/common.js
 * 通过 __atbSetTheme() 覆盖（此时可能有一次极短的切换，但只有「显式选了与系统
 * 相反主题」的用户才会看到）。
 */
(function () {
  var root = document.documentElement;
  var mq = window.matchMedia('(prefers-color-scheme: dark)');
  var pinned = false;

  function paint(dark) {
    root.dataset.resolved = dark ? 'dark' : 'light';
  }

  paint(mq.matches);

  try {
    mq.addEventListener('change', function (e) {
      // 用户显式选了 light/dark 时不跟随系统变化。
      if (!pinned) paint(e.matches);
    });
  } catch (_) {
    // 老内核没有 addEventListener，静默降级为「只按加载时的系统偏好」。
  }

  /**
   * @param {'auto'|'light'|'dark'} mode
   */
  window.__atbSetTheme = function (mode) {
    if (mode === 'light' || mode === 'dark') {
      pinned = true;
      paint(mode === 'dark');
      return;
    }
    pinned = false;
    paint(mq.matches);
  };
})();

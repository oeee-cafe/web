// A drawing on its post page, pinched closer inside its own frame.
//
// The apps hold the page still -- base.jinja keeps the phone and tablet
// web views from pinching, and theme_head.jinja turns the Mac and Windows
// apps' pinch away -- so the drawing does its own: two fingers, a trackpad's
// pinch (a ctrl-wheel in Chromium and WebView2, gesture events in WebKit),
// or a double tap. It never grows past its frame; once closer, a finger,
// the mouse or the wheel moves it about inside, and pinching back out, or
// another double tap, puts it as it was.
//
// When a pinch ends it settles on a whole multiple of the drawing's own
// pixels, as the painter zooms: a fractional step makes some pixels one
// screen pixel wide and some two. And from the moment a pixel of the drawing
// is larger than a pixel of the page it is drawn hard (is-crisp, style.css),
// as the replay's canvas is; smaller, it is scaled down smoothly.
//
// Written for the engines the rest of the site's own scripts are: no arrow
// functions, no pointer events (Firefox 59), nothing a Firefox 56 would
// refuse to parse.
(function () {
  var MOST = 16; // drawing pixels to a page pixel, at the closest

  function attach(stage) {
    var view = stage.querySelector("[data-post-zoom]");
    if (!view) return;
    var layer = view.firstElementChild;
    var natural = parseInt(stage.getAttribute("data-width"), 10);
    if (!layer || !natural) return;

    var scale = 1;
    var x = 0;
    var y = 0;

    function shown() {
      return view.clientWidth || natural;
    }
    // Drawing pixels to a page pixel at `s`.
    function pixels(s) {
      return (shown() * s) / natural;
    }
    function largest() {
      return Math.max(1, (MOST * natural) / shown());
    }

    function clamp(s) {
      return Math.max(1, Math.min(s, largest()));
    }
    // Kept covering the frame: no edge of the drawing comes inside it.
    function keepInside() {
      var width = view.clientWidth;
      var height = view.clientHeight;
      x = Math.min(0, Math.max(x, width - width * scale));
      y = Math.min(0, Math.max(y, height - height * scale));
    }
    function paint(settling) {
      keepInside();
      view.classList.toggle("is-settling", !!settling);
      view.classList.toggle("is-closer", scale > 1);
      stage.classList.toggle("is-crisp", pixels(scale) >= 0.999);
      var transform =
        scale === 1 ? "" : "translate(" + x + "px," + y + "px) scale(" + scale + ")";
      layer.style.webkitTransform = transform;
      layer.style.transform = transform;
    }

    // Scale to `s`, keeping the point of the frame at (px, py) where it is.
    function zoomAt(s, px, py, settling) {
      s = clamp(s);
      x = px - ((px - x) * s) / scale;
      y = py - ((py - y) * s) / scale;
      scale = s;
      paint(settling);
    }
    // The nearest whole step, once closer than the drawing's own size.
    function settle(px, py) {
      var step = pixels(scale);
      if (step < 1) return paint(true);
      var whole = Math.max(1, Math.round(step));
      var s = scale === 1 ? 1 : (whole * natural) / shown();
      if (s < 1.001) s = 1;
      if (px === undefined) {
        px = view.clientWidth / 2;
        py = view.clientHeight / 2;
      }
      zoomAt(s, px, py, true);
    }
    function reset() {
      scale = 1;
      x = 0;
      y = 0;
      paint(true);
    }
    // A double tap or click: closer by a whole step or two, or back.
    function toggleAt(px, py) {
      if (scale > 1) return reset();
      var step = Math.max(2, Math.floor(pixels(1)) * 2);
      zoomAt((step * natural) / shown(), px, py, true);
    }

    function local(clientX, clientY) {
      var box = view.getBoundingClientRect();
      return { x: clientX - box.left, y: clientY - box.top };
    }

    // Touch: two fingers pinch, one moves the drawing once it is closer.
    var pinch = null;
    var pan = null;
    var lastTap = null;
    var moved = false;
    function between(touches) {
      var a = local(touches[0].clientX, touches[0].clientY);
      var b = local(touches[1].clientX, touches[1].clientY);
      return {
        x: (a.x + b.x) / 2,
        y: (a.y + b.y) / 2,
        d: Math.sqrt((a.x - b.x) * (a.x - b.x) + (a.y - b.y) * (a.y - b.y)) || 1
      };
    }
    function startPan(touch) {
      var at = local(touch.clientX, touch.clientY);
      pan = { x: at.x - x, y: at.y - y };
    }
    view.addEventListener("touchstart", function (event) {
      if (event.touches.length >= 2) {
        var mid = between(event.touches);
        pinch = { d: mid.d, scale: scale, cx: (mid.x - x) / scale, cy: (mid.y - y) / scale };
        pan = null;
        moved = true;
        event.preventDefault();
      } else if (event.touches.length === 1) {
        moved = false;
        if (scale > 1) startPan(event.touches[0]);
      }
    }, { passive: false });
    view.addEventListener("touchmove", function (event) {
      if (pinch && event.touches.length >= 2) {
        var mid = between(event.touches);
        scale = clamp((pinch.scale * mid.d) / pinch.d);
        x = mid.x - pinch.cx * scale;
        y = mid.y - pinch.cy * scale;
        paint(false);
        event.preventDefault();
      } else if (pan && event.touches.length === 1) {
        var at = local(event.touches[0].clientX, event.touches[0].clientY);
        x = at.x - pan.x;
        y = at.y - pan.y;
        moved = true;
        paint(false);
        event.preventDefault();
      } else {
        moved = true;
      }
    }, { passive: false });
    view.addEventListener("touchend", function (event) {
      if (pinch && event.touches.length < 2) {
        var last = pinch;
        pinch = null;
        settle(last.cx * scale + x, last.cy * scale + y);
        if (event.touches.length === 1 && scale > 1) startPan(event.touches[0]);
        return;
      }
      if (event.touches.length > 0) return;
      pan = null;
      if (moved || event.changedTouches.length !== 1) {
        lastTap = null;
        return;
      }
      var touch = event.changedTouches[0];
      var now = Date.now();
      if (lastTap && now - lastTap.time < 300 &&
          Math.abs(touch.clientX - lastTap.x) < 30 && Math.abs(touch.clientY - lastTap.y) < 30) {
        lastTap = null;
        var at = local(touch.clientX, touch.clientY);
        toggleAt(at.x, at.y);
        // No click, nor the double click Chrome would make of it.
        event.preventDefault();
      } else {
        lastTap = { time: now, x: touch.clientX, y: touch.clientY };
      }
    });
    view.addEventListener("touchcancel", function () {
      if (pinch) settle();
      pinch = null;
      pan = null;
    });

    // A trackpad's pinch in WebKit. iOS sends these alongside the touches
    // above, which have it already.
    var gesture = null;
    view.addEventListener("gesturestart", function (event) {
      event.preventDefault();
      if (pinch) return;
      var at = local(event.clientX, event.clientY);
      gesture = { scale: scale, x: at.x, y: at.y };
    });
    view.addEventListener("gesturechange", function (event) {
      event.preventDefault();
      if (!gesture || pinch) return;
      zoomAt(gesture.scale * event.scale, gesture.x, gesture.y, false);
    });
    view.addEventListener("gestureend", function (event) {
      event.preventDefault();
      if (!gesture || pinch) return;
      var at = gesture;
      gesture = null;
      settle(at.x, at.y);
    });

    // A trackpad's pinch everywhere else, and Ctrl with the wheel; once
    // closer, the wheel moves the drawing, until it reaches an edge.
    var wheelEnd = null;
    view.addEventListener("wheel", function (event) {
      var unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? view.clientHeight : 1;
      var at = local(event.clientX, event.clientY);
      if (event.ctrlKey) {
        event.preventDefault();
        zoomAt(scale * Math.exp((-event.deltaY * unit) / 100), at.x, at.y, false);
        clearTimeout(wheelEnd);
        wheelEnd = setTimeout(function () {
          settle(at.x, at.y);
        }, 200);
        return;
      }
      if (scale === 1) return;
      var beforeX = x;
      var beforeY = y;
      x -= event.deltaX * unit;
      y -= event.deltaY * unit;
      paint(false);
      if (x !== beforeX || y !== beforeY) event.preventDefault();
    }, { passive: false });

    // The mouse moves the drawing once it is closer.
    var drag = null;
    view.addEventListener("mousedown", function (event) {
      if (event.button !== 0 || scale === 1) return;
      drag = { x: event.clientX - x, y: event.clientY - y };
      view.classList.add("is-dragging");
      event.preventDefault();
    });
    window.addEventListener("mousemove", function (event) {
      if (!drag) return;
      x = event.clientX - drag.x;
      y = event.clientY - drag.y;
      paint(false);
    });
    window.addEventListener("mouseup", function () {
      if (!drag) return;
      drag = null;
      view.classList.remove("is-dragging");
    });
    view.addEventListener("dblclick", function (event) {
      var at = local(event.clientX, event.clientY);
      toggleAt(at.x, at.y);
      event.preventDefault();
    });
    view.addEventListener("dragstart", function (event) {
      event.preventDefault();
    });

    // A new size of window is a new frame: as it was, and sharp or not
    // for the size the page now shows it at.
    window.addEventListener("resize", reset);
    paint(false);
  }

  var stages = document.querySelectorAll("[data-post-stage]");
  for (var i = 0; i < stages.length; i++) attach(stages[i]);
})();

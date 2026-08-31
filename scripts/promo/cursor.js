/*
 * A DRAWN cursor, injected into the app page during a promo recording.
 *
 * Why draw one instead of recording the real pointer: CDP's screencast does not
 * composite the OS cursor into its frames at all, so a recording made this way
 * has no pointer whatsoever. Drawing it is not a workaround — it is the better
 * artifact. A recorded human cursor jitters, over-shoots and micro-corrects; a
 * drawn one moves on an exact eased path (slow out, fast middle, slow in), which
 * is the "superhuman but smooth" motion the Linear/Raycast product videos use to
 * lead the eye. See docs/research/2026-08-31-app-promo-video.md §1.4.
 *
 * The element is `position: fixed` and appended to <body>, so it never becomes a
 * containing block for anything in the app (see angular-zoneless.md's teleport /
 * containing-block trap) and never takes pointer events. The REAL mouse is moved
 * separately by the driver so hover states still fire — this is only the picture.
 */
(() => {
  const ACCENT = "#6e76ff";
  let root = null;
  let ring = null;

  const ARROW = `
    <svg width="26" height="30" viewBox="0 0 26 30" aria-hidden="true"
         style="position:absolute;left:-1px;top:-1px;filter:drop-shadow(0 4px 7px rgba(0,0,0,.55))">
      <path d="M4 2.4 L4 22.4 L9.1 17.6 L12.5 25.6 L16.2 24 L12.8 16.2 L19.6 15.6 Z"
            fill="#ffffff" stroke="#0b0b12" stroke-width="1.35" stroke-linejoin="round"/>
    </svg>`;

  function build() {
    if (root && root.isConnected) return;
    root = document.createElement("div");
    root.id = "__promo_cursor";
    root.style.cssText = [
      "position:fixed",
      "left:0",
      "top:0",
      "width:0",
      "height:0",
      "z-index:2147483647",
      "pointer-events:none",
      "opacity:0",
      "transform:translate3d(-300px,-300px,0)",
      "will-change:transform",
    ].join(";");
    root.innerHTML = ARROW;

    // The click ripple: a ring that expands and fades from the pointer tip.
    ring = document.createElement("div");
    ring.style.cssText = [
      "position:absolute",
      "left:0",
      "top:0",
      "width:10px",
      "height:10px",
      "margin:-5px 0 0 -5px",
      "border-radius:999px",
      `border:2px solid ${ACCENT}`,
      "opacity:0",
      "will-change:transform,opacity",
    ].join(";");
    root.appendChild(ring);

    document.body.appendChild(root);
  }

  window.__promoCursor = {
    /** Install the cursor (idempotent; call again after a navigation). */
    init(x = -300, y = -300) {
      build();
      root.style.transition = "none";
      root.style.transform = `translate3d(${x}px,${y}px,0)`;
      // Force the no-transition placement to commit before anything animates it.
      void root.offsetWidth;
    },

    show(on = true) {
      build();
      root.style.transition = "opacity 220ms linear";
      root.style.opacity = on ? "1" : "0";
    },

    /**
     * Glide to (x, y) over `ms`. The curve is deliberately exaggerated —
     * cubic-bezier(.62,0,.16,1) is a hard ease-in-out that leaves slowly, covers
     * the distance fast, and settles gently.
     */
    moveTo(x, y, ms = 520) {
      build();
      root.style.transition = `transform ${ms}ms cubic-bezier(.62,0,.16,1), opacity 220ms linear`;
      root.style.transform = `translate3d(${x}px,${y}px,0)`;
    },

    /** The click tell: a quick dip of the arrow plus an expanding ring. */
    click() {
      build();
      ring.animate(
        [
          { transform: "scale(.35)", opacity: 0.9 },
          { transform: "scale(4.2)", opacity: 0 },
        ],
        { duration: 460, easing: "cubic-bezier(.2,.6,.2,1)" },
      );
      const svg = root.querySelector("svg");
      if (svg) {
        svg.animate([{ transform: "scale(1)" }, { transform: "scale(.82)" }, { transform: "scale(1)" }], {
          duration: 200,
          easing: "ease-out",
        });
      }
    },
  };
})();

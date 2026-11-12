/* Backdrop: a field of light threads (WebGL2) behind the screen on the stage. The
   renderer sleeps whenever it is hidden or offscreen; without WebGL2 the grid stays. */

/* ---------- shaders ---------- */

const HEAD = `#version 300 es
precision highp float;
precision highp int;
`;

const COMMON = `
uniform vec4 uMap;     // cos angle, sin angle, tilt (rad), perspective (CSS px)
uniform vec4 uMap2;    // CSS px per world unit, content centre x, y (CSS px), device px per CSS px
uniform vec2 uFrame;   // frame size, CSS px
uniform vec4 uEye;     // eyelet x, y, reach R, gather a
uniform vec4 uEye2;    // waist sigma, lean (world), lag (world), focus y
uniform vec4 uDof;     // sharp half-band, blur gain (world), hairline sigma (CSS px), min sigma (CSS px)
uniform vec4 uSway;    // amplitude (world), phase, parallax x, parallax y (world per unit depth)
uniform vec4 uWave[4];  // centre x, y, radius, strength (0 = idle)
uniform vec4 uWaveP;   // ring width, push (world), brightening, -
uniform float uSalt;
uniform float uTilt;   // how far the gathered threads lean through the waist (dx/dy)
uniform float uBundleShift; // slow sideways drift of the bright bundles (world)
uniform vec4 uLamp[6];  // a visitor's lamp: x, y (world), 1 / reach^2, brightness (0 = none)
const float PULL_K = ${(1 / (1.6 * 1.6)).toFixed(5)};

uint hu(uint x) { x ^= x >> 16; x *= 0x7feb352du; x ^= x >> 15; x *= 0x846ca68bu; x ^= x >> 16; return x; }
vec4 h4(uint x) {
    uint a = hu(x); uint b = hu(a ^ 0x9e3779b9u); uint c = hu(b ^ 0x85ebca6bu); uint d = hu(c ^ 0xc2b2ae35u);
    return vec4(float(a >> 8u), float(b >> 8u), float(c >> 8u), float(d >> 8u)) * (1.0 / 16777216.0);
}
float h1(int i) { return float(hu(uint(i + 1048576) * 0x2c1b3c6du + uint(uSalt) * 0x297a2d39u) >> 8u) * (1.0 / 16777216.0); }
// Bundles: a slow 1-D noise across the sheet, so some runs of threads burn white
// and the lanes between them stay copper.
float bundleAt(float x0) {
    x0 += uBundleShift;
    float s = x0 * 26.0;
    float i = floor(s);
    float f = s - i;
    f = f * f * (3.0 - 2.0 * f);
    float n = mix(h1(int(i)), h1(int(i) + 1), f);
    float s2 = x0 * 71.0 + 13.0;
    float i2 = floor(s2);
    float f2 = s2 - i2;
    f2 = f2 * f2 * (3.0 - 2.0 * f2);
    n = 0.7 * n + 0.3 * mix(h1(int(i2) + 7777), h1(int(i2) + 7778), f2);
    return 0.15 + 2.2 * n * n * n;
}

// Where thread x0 runs at height y. P: how strongly the eyelet reaches it,
// Q: how near the waist this height is, c: how far it has been drawn in.
// d: the thread's slope dx/dy there (analytic, so a vertex needs one evaluation),
// dc: how fast c changes with y.
struct TP { vec2 p; float P; float Q; float c; float d; float dc; };
TP threadAt(float x0, float y, float gm) {
    TP t;
    float R = max(uEye.z, 1e-3);
    float dx = x0 - uEye.x;
    float u = dx / R;
    float u2 = u * u;
    t.P = exp(-u2 * u2);
    // The pull reaches further than the light, so the bright bundle cinches as one
    // and the dim threads just outside it bow in toward the eyelet too.
    float v2 = u2 * PULL_K;
    float yc = uEye.y - uEye2.y * tanh(u);
    float g = (y - yc) / max(uEye2.x, 1e-3);
    t.Q = 1.0 / (1.0 + g * g);
    float A = uEye.w * gm * exp(-v2 * v2);
    t.c = A * t.Q;
    float off = uEye2.z * (1.0 - t.Q) + uTilt * (y - yc);
    t.p = vec2(x0 - t.c * dx + t.c * off, y);
    float dQ = -2.0 * g / max(uEye2.x, 1e-3) * t.Q * t.Q;
    t.dc = A * dQ;
    t.d = t.dc * (off - dx) + t.c * (uTilt - uEye2.z * dQ);
    return t;
}
// Shock rings: push threads sideways off each ring and brighten them as it passes.
// sl carries the thread's slope dx/dy through the push.
float shockAt(inout vec2 p, inout float sl) {
    float b = 0.0;
    float dxs = 0.0;
    float dsl = 0.0;
    for (int k = 0; k < 4; k++) {
        vec4 wv = uWave[k];
        if (wv.w <= 0.0) continue;
        vec2 d = p - wv.xy;
        float r = max(length(d), 1e-3);
        float g = (r - wv.z) / uWaveP.x;
        float ring = wv.w * exp(-g * g);
        float g2 = g + 1.6;
        float lag = 0.45 * wv.w * exp(-g2 * g2);
        // A short wave train: the ring leads, a softer counter-swing follows it.
        float train = ring - lag;
        float dtrain = (-2.0 * g * ring + 2.0 * g2 * lag) / uWaveP.x;
        float drdy = (d.x * sl + d.y) / r;
        float q = d.x / r;
        float dq = (sl * r - d.x * drdy) / (r * r);
        dxs += uWaveP.y * train * q;
        dsl += uWaveP.y * (dtrain * drdy * q + train * dq);
        b += ring;
    }
    p.x += dxs;
    sl += dsl;
    return b;
}
// Field light added by the visitors' lamps at world point p.
float lampAt(vec2 p) {
    float s = 0.0;
    for (int k = 0; k < 6; k++) {
        vec4 l = uLamp[k];
        if (l.w <= 0.0) continue;
        vec2 d = p - l.xy;
        s += l.w * exp(-dot(d, d) * l.z);
    }
    return s;
}
float dofAt(float y) {
    float d = max(abs(y - uEye2.w) - uDof.x, 0.0);
    return uDof.y * pow(d, 1.3);
}
// How far into the open, defocused ends a height lies (0 in the focus band, 1 beyond it).
float endsAt(float y) {
    return smoothstep(0.03, 0.2, max(abs(y - uEye2.w) - uDof.x, 0.0));
}
vec2 toScreen(vec2 w) {
    vec2 q = w * uMap2.x;
    vec2 r = vec2(q.x * uMap.x - q.y * uMap.y, q.x * uMap.y + q.y * uMap.x);
    float k = uMap.w / max(uMap.w - r.y * sin(uMap.z), 1.0);
    return uMap2.yz + vec2(r.x, r.y * cos(uMap.z)) * k;
}
vec2 toWorld(vec2 s) {
    vec2 v = s - uMap2.yz;
    float st = sin(uMap.z);
    float ry = v.y * uMap.w / max(cos(uMap.z) * uMap.w + v.y * st, 1.0);
    float k = uMap.w / max(uMap.w - ry * st, 1.0);
    vec2 r = vec2(v.x / k, ry);
    vec2 q = vec2(r.x * uMap.x + r.y * uMap.y, -r.x * uMap.y + r.y * uMap.x);
    return q / uMap2.x;
}
vec4 toClip(vec2 s) {
    return vec4(s.x / uFrame.x * 2.0 - 1.0, 1.0 - s.y / uFrame.y * 2.0, 0.0, 1.0);
}
// Screen-space direction of a thread with slope sl at world point p.
vec2 tangentAt(vec2 p, vec2 s0, float sl) {
    vec2 tg = toScreen(p + 0.002 * vec2(sl, 1.0)) - s0;
    return tg / max(length(tg), 1e-5);
}
`;

const THREAD_VS = HEAD + COMMON + `
uniform vec4 uTh;    // first thread index, stride, pitch (world), brightness per thread
uniform vec4 uTh2;   // y from, y to (world), gather share, layer (0 sharp sheet, 1 near strands)
uniform vec4 uTh3;   // flow offset, fibre gain, bed gain, field light
uniform vec4 uTh4;   // fringe base (CSS px), fringe per blur px, near blur (CSS px), near gain
uniform vec4 uTh5;   // whitening, time, flicker, -
uniform vec3 uK0;    // white core,
uniform vec3 uK1;    // warm,
uniform vec3 uK2;    // cyan and
uniform vec3 uK3;    // violet fibres as light; in focus the cool ones read near-white:
uniform vec3 uK2s;
uniform vec3 uK3s;
uniform vec3 uMidE;
uniform vec3 uTint;  // copper the dim light leans to
out vec4 vA;
out vec3 vE;
out float vS;        // scrolled position along the thread
flat out vec4 vK;    // dash frequencies k1, k2, k3, gate
flat out vec4 vPh;   // dash phases, near flag
uniform float uSeg;  // segments per thread in this draw
void main() {
    int SEG = int(uSeg);
    int VPT = 2 * (SEG + 1) + 2;
    int tid = gl_VertexID / VPT;
    int v = gl_VertexID - tid * VPT;
    int j = clamp(v - 1, 0, 2 * SEG + 1);
    int row = j / 2;
    float side = (j - row * 2) == 0 ? -1.0 : 1.0;
    int idx = int(uTh.x) + tid * int(uTh.y);
    bool nearL = uTh2.w > 0.5;
    uint hid = uint(idx + 1048576) * (nearL ? 0x632be5abu : 0x9E3779B1u) + uint(uSalt) * 0x85ebca6bu;
    vec4 h = h4(hid);
    vec4 h2 = h4(hu(hid ^ 0x5bd1e995u));
    vec4 h3 = h4(hu(hid ^ 0x27d4eb2du));
    float x0 = (float(idx) + 0.5 + (h.x - 0.5) * 0.85) * uTh.z;
    // Each sheet thread wanders a little sideways on its own slow beat, so the
    // crossings where threads bunch at the waist never sit still.
    if (!nearL) x0 += 0.3 * uTh.z * uTh.y * sin(6.2831853 * (uTh5.y * (0.04 + 0.08 * h3.y) + h.x));
    // Depth: the sheet is nearly flat; near strands sit well in front of it.
    float z = nearL ? 0.55 + 0.45 * h2.x : 0.12 * h2.x;
    x0 += uSway.z * z;
    float y = mix(uTh2.x, uTh2.y, float(row) / float(SEG));
    float sw = uSway.x * (0.6 + 0.8 * h3.x);
    float swf = 2.2 + 2.5 * h3.y;
    TP t = threadAt(x0, y, uTh2.z);
    vec2 p = t.p;
    float sl = t.d;
    float sa = y * swf + uSway.y * (0.7 + 0.6 * h3.z) + 6.2831853 * h3.w;
    float sv = sin(sa);
    p.x += sw * sv * (1.0 - t.c);
    sl += sw * (cos(sa) * swf * (1.0 - t.c) - sv * t.dc);
    p.y += uSway.w * z;
    float ring = shockAt(p, sl);
    vec2 s0 = toScreen(p);
    vec2 tg = tangentAt(p, s0, sl);
    vec2 n = vec2(-tg.y, tg.x);

    // Brightening and fading along the length, scrolling up with the flow.
    bool bright = nearL || h.y < 0.38;
    float s = (y + uTh3.x * (0.8 + 0.4 * h2.y)) * 0.5;
    float k1 = 1.0 + floor(h2.z * 3.0);
    float k2 = 4.0 + floor(h2.w * 6.0);
    float k3 = 11.0 + floor(h3.z * 14.0);
    // The dashes are worked out per pixel (THREAD_FS). Sampled at the vertices, the
    // fine ones landed on the fixed vertex rows and pulsed in place instead of travelling.
    vS = s;
    vK = vec4(k1, k2, k3, bright ? 0.02 : 0.35);
    vPh = vec4(h3.x, h3.y, h3.w, nearL ? 1.0 : 0.0);
    float fl = 1.0 - uTh5.z + uTh5.z * (0.5 + 0.5 * sin(6.2831853 * (uTh5.y * (0.15 + 0.4 * h3.y) + h2.x)));
    float amp = bright ? 0.35 + 0.65 * h.w : 0.2 + 0.6 * h.w;

    float dofW = dofAt(y);
    float dofPx = dofW * uMap2.x;
    float nearPx = nearL ? uTh4.z * (0.45 + 0.55 * z) : 0.0;
    float hair = max(uDof.z, uDof.w);
    float sig = sqrt(hair * hair + dofPx * dofPx + nearPx * nearPx);
    float delta = (uTh4.x + uTh4.y * (dofPx + nearPx)) * (p.x < uEye.x ? -1.0 : 1.0);
    float ends = endsAt(y);
    // Gathered threads are the bright ones; the rest of the sheet glows low.
    float lampL = lampAt(p);
    float reach = mix(min(uTh3.w + lampL, 1.0), 1.0, t.P);
    // Where the eyelet bunches them the light whitens; beyond it, it leans copper.
    float wh = smoothstep(0.35, 0.85, t.c) * mix(0.5, 1.0, ends) * uTh5.x;
    float r = h.z;
    vec3 col;
    if (!bright) col = uMidE * uTh3.z * max(t.P, 0.5 * lampL);
    else if (r < 0.4) col = uK0;
    else if (r < 0.62) col = mix(uK1, uK0, wh);
    else if (r < 0.82) col = mix(uK2s, uK2, max(ends, float(nearL)));
    else col = mix(uK3s, uK3, max(ends, float(nearL)));
    if (bright) col *= uTh3.y;
    col *= mix(uTint, vec3(1.0), max(t.P, wh));
    // White-hot where the eyelet bunches them.
    float e = amp * fl * reach * bundleAt(x0) * uTh.w * (hair / sig) * (1.0 + 1.5 * t.c) * (1.0 + uWaveP.z * ring);
    // Out of focus, a strand's light spreads rather than fades as fast as its width grows.
    if (nearL) e *= uTh4.w * pow(sig / hair, 0.7);
    vE = col * e;
    // The gaussian is spent by 2.7σ. Stretches with no light to show (dark gaps in a
    // fibre, bed threads beyond the reach) collapse to zero width and cost no pixels.
    float ext = max(vE.r, max(vE.g, vE.b)) < 0.003 ? 0.0 : 2.7 * sig + abs(delta) + 0.75;
    gl_Position = toClip(s0 + n * side * ext);
    vA = vec4(side * ext, sig, delta, 0.0);
}
`;

const THREAD_FS = HEAD + `
in vec4 vA;
in vec3 vE;
in float vS;
flat in vec4 vK;
flat in vec4 vPh;
uniform float uOut;
out vec4 o;
void main() {
    // Brightening and fading along the length, scrolling up with the flow.
    float along;
    if (vPh.w > 0.5) {
        // Near strands are long soft veils, not dashes: one slow swell along each.
        along = 0.25 + 0.75 * (0.5 + 0.5 * sin(6.2831853 * (vK.x * vS * 0.5 + vPh.x)));
    } else {
        float nn = 0.5 * sin(6.2831853 * (vK.x * vS + vPh.x)) + 0.32 * sin(6.2831853 * (vK.y * vS + vPh.y)) + 0.18 * sin(6.2831853 * (vK.z * vS + vPh.z));
        float tt = clamp((nn + vK.w) / 0.8, 0.0, 1.0);
        along = tt * tt * (3.0 - 2.0 * tt);
    }
    float s = vA.y;
    vec3 d = vec3(vA.x - vA.z, vA.x, vA.x + vA.z);
    vec3 E = vE * along * exp(-0.5 * d * d / (s * s));
    o = vec4(clamp(E, vec3(0.0), vec3(64.0)) * uOut, 0.0);
}
`;

const MAGPIE_VS = HEAD + COMMON + `
uniform vec4 uGl;   // first lane, lanes, slots per lane, lane pitch (world)
uniform vec4 uGl2;  // range start, range length, flow offset, occupancy
uniform vec4 uGl3;  // hairline sigma (CSS px), gain, cyan share, violet share
uniform vec4 uGl4;  // flicker, time, fringe per blur px, fringe base px
uniform vec4 uGl5;  // field light, length scale, lane stride, -
uniform vec3 uCoreE;
uniform vec3 uBaseE;
uniform vec3 uAccE;
uniform vec3 uVioE;
out vec4 vA;
out float vSc;
out vec3 vCore;
out vec3 vHalo;
out float vI;
const int SEG = 10;
void main() {
    int corner = gl_VertexID % 6;
    int inst = gl_VertexID / (6 * SEG);
    int seg = (gl_VertexID / 6) - inst * SEG;
    int rowOff = (corner == 2 || corner == 4 || corner == 5) ? 1 : 0;
    float side = (corner == 1 || corner == 3 || corner == 5) ? 1.0 : -1.0;
    int NS = int(uGl.z);
    int lane = inst / NS;
    int slot = inst - lane * NS;
    int li = int(uGl.x) + lane * int(uGl5.z);
    uint sd = uint(uSalt) * 0x9E3779B1u;
    vec4 hl = h4(uint(li + 1048576) * 0x27d4eb2du + sd);
    float x0 = (float(li) + 0.5 + (hl.x - 0.5) * 0.9) * uGl.w;
    float P = uGl2.y;
    float cell = P / float(NS);
    // Upward flow: the moving coordinate falls with time and wraps over the range.
    float m = (float(slot) + hl.y) * cell - uGl2.z * (0.8 + 0.4 * hl.z);
    float cyc = floor(m / P);
    float mw = m - cyc * P;
    uint cu = uint(int(cyc) + 65536);
    uint iid = uint(li + 1048576) * 64u + uint(slot);
    vec4 hs = h4(iid * 0x165667b1u + cu * 0x61c88647u + sd);
    vec4 hs2 = h4(hu(iid ^ 0x5bd1e995u) + cu * 0x2545f491u + sd);
    float yc = uGl2.x + mw;
    TP tc = threadAt(x0, yc, 1.0);
    float reach = mix(min(uGl5.x + lampAt(tc.p), 1.0), 1.0, tc.P);
    float fade = smoothstep(0.0, 0.25, mw) * smoothstep(0.0, 0.25, P - mw);
    // Glints crowd the gathered threads; out in the sheet they are rarer.
    if (hs.x > uGl2.w * mix(0.22, 1.0, tc.P) || reach * fade < 0.004) { gl_Position = vec4(2.0, 2.0, 2.0, 1.0); return; }

    // Wide clumps gather in the middle of the bundle; the flanks carry only slender spindles.
    float flank = smoothstep(0.5, 1.4, abs(x0 - uEye.x) / max(uEye.z, 1e-3));
    float wq = hs.w * (1.0 - 0.75 * flank);
    float spread = 1.0 - 0.85 * tc.c;
    float len = 0.25 * (0.12 + 0.5 * hs.y * hs.y) * (1.0 + 2.0 * wq * wq) * pow(max(spread, 0.3), 0.5) * uGl5.y * (1.0 + 2.6 * endsAt(yc));

    float tau = float(seg + rowOff) / float(SEG);
    float y = yc + (tau - 0.5) * len;
    TP t = threadAt(x0, y, 1.0);
    vec2 p = t.p;
    float sl = t.d;
    float ring = shockAt(p, sl);
    vec2 s0 = toScreen(p);
    vec2 tg = tangentAt(p, s0, sl);
    vec2 n = vec2(-tg.y, tg.x);
    float a = sin(3.14159265 * tau);
    a *= a;
    float dofPx = dofAt(y) * uMap2.x;
    // Each spindle has its own width, from a hairline to a soft clump; drawn in
    // by the eyelet it narrows with the threads around it.
    float wW = 0.0019 * pow(12.0, wq) * (1.0 - 0.95 * t.c);
    float body = max(uGl3.x, wW * uMap2.x) * (0.35 + 0.65 * sqrt(a));
    float sig = sqrt(body * body + dofPx * dofPx);
    vSc = sqrt(uGl3.x * uGl3.x + dofPx * dofPx);
    float delta = (uGl4.w + uGl4.z * dofPx) * (x0 < uEye.x ? -1.0 : 1.0);
    float ext = 3.0 * sig + abs(delta) + 1.0;
    gl_Position = toClip(s0 + n * side * ext);
    vA = vec4(side * ext, tau, sig, delta);
    float fl = 1.0 - uGl4.x + uGl4.x * (0.5 + 0.5 * sin(6.2831853 * (uGl4.y * (0.15 + 0.5 * hs2.x) + hs2.y)));
    float wht = 0.6 + 0.8 * smoothstep(0.3, 0.8, tc.c);
    vI = uGl3.y * reach * bundleAt(x0) * wht * fade * fl * (0.25 + 0.75 * hs.z * hs.z) * (body / sig) * (1.0 + uWaveP.z * ring);
    // Cool fringes belong to the defocused ends: none at the waist.
    float ends = endsAt(yc);
    float cool = 2.0 * ends;
    float hj = hs2.w - 0.5;
    vec3 halo = uBaseE * vec3(1.0, 1.0 + 0.9 * hj, 1.0 + 1.2 * hj);
    vec3 core = uCoreE;
    float slim = 1.0 - smoothstep(0.35, 0.6, wq);
    if (hs2.z < uGl3.z * cool * slim) { halo = uAccE; core = mix(uCoreE, uAccE, 0.7); }
    else if (hs2.z < (uGl3.z + uGl3.w) * cool * slim) { halo = uVioE; core = mix(uCoreE, uVioE, 0.7); }
    float clump = smoothstep(0.5, 0.85, wq);
    vCore = core * (uGl3.x / vSc) * 0.6;
    vHalo = mix(halo, core * 0.6, clump);
}
`;

const MAGPIE_FS = HEAD + `
in vec4 vA;
in float vSc;
in vec3 vCore;
in vec3 vHalo;
in float vI;
uniform float uOut;
out vec4 o;
void main() {
    float a = sin(3.14159265 * vA.y);
    a *= a;
    float s = max(vA.z, 0.3);
    float sc = max(min(vSc, s), 0.3);
    vec3 d = vec3(vA.x - vA.w, vA.x, vA.x + vA.w);
    vec3 E = vI * a * (vCore * exp(-0.5 * d * d / (sc * sc)) * sqrt(a) + vHalo * exp(-0.5 * d * d / (s * s)));
    o = vec4(clamp(E, vec3(0.0), vec3(64.0)) * uOut, 0.0);
}
`;

const DUST_VS = HEAD + COMMON + `
uniform vec4 uDu;   // range start, range length, flow offset, point size (device px)
uniform vec4 uDu2;  // eyelet-cluster count, sheet x from, sheet x span, strength
uniform vec4 uDu3;  // time, field light, -, -
uniform vec3 uDustE;
out vec3 vC;
void main() {
    int id = gl_VertexID;
    uint sd = uint(uSalt) * 0x297a2d39u;
    vec4 h = h4(uint(id) * 0x2c1b3c6du + sd);
    vec4 h2 = h4(hu(uint(id) ^ 0x51ed270bu) + sd);
    float x0;
    if (float(id) < uDu2.x) {
        // Box-Muller: a gaussian cloud the size of the reach, centred on the eyelet.
        float g = sqrt(-2.0 * log(max(h.w, 1e-6))) * cos(6.2831853 * h2.z);
        x0 = uEye.x + 0.7 * uEye.z * g;
    } else {
        x0 = uDu2.y + uDu2.z * h.w;
    }
    float P = uDu.y;
    float m = h.y * P - uDu.z * (0.25 + 0.15 * h.z);
    float mw = m - floor(m / P) * P;
    float y = uDu.x + mw;
    TP t = threadAt(x0, y, 1.0);
    vec2 p = t.p;
    float sl = t.d;
    float ring = shockAt(p, sl);
    vec2 s = toScreen(p);
    float tw = max(0.0, 0.55 + 0.45 * sin(6.2831853 * (uDu3.x * (0.3 + 1.4 * h2.z) + h2.w)));
    float fade = smoothstep(0.0, 0.2, mw) * smoothstep(0.0, 0.2, P - mw);
    float b = uDu2.w * tw * fade * (0.25 + 0.75 * h.z) * mix(min(uDu3.y + lampAt(p), 1.0), 1.0, t.P) * (1.0 + 0.6 * t.c) * (1.0 + 2.0 * ring);
    gl_PointSize = uDu.w * (0.8 + 0.7 * h2.x * h2.x);
    gl_Position = b < 0.003 ? vec4(2.0, 2.0, 2.0, 1.0) : toClip(s);
    vC = uDustE * b;
}
`;

const SPARK_VS = HEAD + COMMON + `
uniform vec4 uSp;   // thread x0 at the click, click y, age (s), strength
uniform vec4 uSp2;  // point size (device px), wave id, flow offset, -
uniform vec3 uHot;
uniform vec3 uWarm;
out vec3 vC;
void main() {
    int id = gl_VertexID;
    uint sd = uint(uSalt) * 0x7feb352du + uint(uSp2.y) * 0x9e3779b9u;
    vec4 h = h4(uint(id) * 0x2c1b3c6du + sd);
    vec4 h2 = h4(hu(uint(id) ^ 0x51ed270bu) + sd);
    float g = sqrt(-2.0 * log(max(h.x, 1e-6))) * cos(6.2831853 * h.y);
    float x0 = uSp.x + g * (0.012 + 0.03 * h2.w);
    float dir = h.z < 0.5 ? -1.0 : 1.0;
    float v0 = 0.25 + 1.25 * h.w * h.w;
    float tau = 0.28 + 0.25 * h2.x;
    float age = uSp.z;
    float y = uSp.y + dir * v0 * tau * (1.0 - exp(-age / tau)) - 0.05 * age;
    TP t = threadAt(x0, y, 1.0);
    vec2 p = t.p;
    vec2 s = toScreen(p);
    float life = 0.55 + 1.1 * h2.y;
    float k = clamp(age / life, 0.0, 1.0);
    float tw = 0.6 + 0.4 * sin(age * (14.0 + 20.0 * h2.z) + 6.2831853 * h2.x);
    float b = uSp.w * (1.0 - k) * (1.0 - k) * tw * (0.4 + 0.6 * h2.z);
    gl_PointSize = uSp2.x * (1.0 + 1.2 * (1.0 - k) * h.w);
    gl_Position = (b < 0.003 || age > life) ? vec4(2.0, 2.0, 2.0, 1.0) : toClip(s);
    vC = mix(uWarm, uHot, (1.0 - k) * (1.0 - k)) * b;
}
`;

const POINT_FS = HEAD + `
in vec3 vC;
uniform float uOut;
out vec4 o;
void main() {
    vec2 r = gl_PointCoord - 0.5;
    o = vec4(vC * exp(-dot(r, r) * 5.0) * uOut, 0.0);
}
`;

const FULL_VS = HEAD + `
in vec2 aPos;
void main() { gl_Position = vec4(aPos, 0.0, 1.0); }
`;

const HAZE_FS = HEAD + COMMON + `
uniform vec2 uBuf;
uniform float uPxc;     // device px per CSS px in this target
uniform vec4 uHazeE;    // haze light (rgb), strength
uniform vec3 uRingE;
uniform float uOut;
out vec4 o;
float gx(float dx, float y) {
    float R = max(uEye.z, 1e-3);
    float u = dx / R;
    float u2 = u * u * PULL_K;
    float P = exp(-u2 * u2);
    float yc = uEye.y - uEye2.y * tanh(u);
    float g = (y - yc) / max(uEye2.x, 1e-3);
    float Q = 1.0 / (1.0 + g * g);
    float c = uEye.w * P * Q;
    return dx - c * dx + c * (uEye2.z * (1.0 - Q) + uTilt * (y - yc));
}
void main() {
    vec2 css = vec2(gl_FragCoord.x, uBuf.y - gl_FragCoord.y) / uPxc;
    vec2 w = toWorld(css);
    float R = max(uEye.z, 1e-3);
    float X = w.x - uEye.x;
    vec3 E = vec3(0.0);
    if (abs(X) < 4.0 * R) {
        // Newton on the monotonic map, from the waist's compression as a first guess.
        float g0 = (w.y - uEye.y) / max(uEye2.x, 1e-3);
        float dx = clamp(X / max(1.0 - uEye.w / (1.0 + g0 * g0), 0.05), -3.5 * R, 3.5 * R);
        float hh = 0.002 * R;
        // Newton on dx in [-4R, 4R]: the map is monotonic there.
        float d1 = 1.0;
        for (int i = 0; i < 4; i++) {
            float f = gx(dx, w.y) - X;
            d1 = max((gx(dx + hh, w.y) - gx(dx, w.y)) / hh, 0.03);
            dx = clamp(dx - f / d1, -4.0 * R, 4.0 * R);
        }
        float u = dx / R;
        float P = exp(-u * u * u * u);
        float dens = min(P / d1, 12.0);
        float ends = endsAt(w.y);
        E = uHazeE.rgb * uHazeE.w * pow(max(dens, 0.0), 0.8) * mix(1.0, 0.3, ends);
    }
    float rg = 0.0;
    for (int k = 0; k < 4; k++) {
        vec4 wv = uWave[k];
        if (wv.w <= 0.0) continue;
        float g = (length(w - wv.xy) - wv.z) / (uWaveP.x * 1.1);
        rg += wv.w * exp(-g * g);
    }
    E += uRingE * rg;
    if (any(isnan(E)) || any(isinf(E))) E = vec3(0.0);
    o = vec4(min(E, vec3(64.0)) * uOut, 0.0);
}
`;

const DOWN_FS = HEAD + `
uniform sampler2D uSrc;
uniform vec2 uSrcPx;
uniform vec2 uDstRes;
out vec4 o;
void main() {
    vec2 uv = gl_FragCoord.xy / uDstRes;
    vec2 d = uSrcPx;
    vec4 c = 0.25 * (texture(uSrc, uv + vec2(-d.x, -d.y)) + texture(uSrc, uv + vec2(d.x, -d.y)) +
                texture(uSrc, uv + vec2(-d.x, d.y)) + texture(uSrc, uv + d));
    // Anything that feeds a blur is untrusted: one NaN would spread into a block.
    if (any(isnan(c)) || any(isinf(c))) c = vec4(0.0);
    o = c;
}
`;

const BLUR_FS = HEAD + `
uniform sampler2D uSrc;
uniform vec2 uStep;
uniform vec2 uDstRes;
out vec4 o;
void main() {
    vec2 uv = gl_FragCoord.xy / uDstRes;
    vec4 acc = texture(uSrc, uv) * 0.16;
    float w[5] = float[5](0.1478, 0.1078, 0.0688, 0.0384, 0.0187);
    for (int i = 0; i < 5; i++) {
        vec2 off = uStep * float(i + 1);
        acc += (texture(uSrc, uv + off) + texture(uSrc, uv - off)) * w[i];
    }
    o = acc / (0.16 + 2.0 * 0.3815);
}
`;

const COMPOSITE_FS = HEAD + `
uniform sampler2D uScene;
uniform sampler2D uBloomA;
uniform sampler2D uBloomB;
uniform sampler2D uSoft;
uniform vec2 uBuf;
uniform vec4 uLook;    // exposure, bloom, scene scale, device px per CSS px
uniform vec4 uBg;      // ground (straight rgb), alpha
uniform vec4 uFx;      // grain, vignette, stars, time
uniform vec3 uDustE;
uniform float uSeed;
out vec4 o;
uint hu(uint x) { x ^= x >> 16; x *= 0x7feb352du; x ^= x >> 15; x *= 0x846ca68bu; x ^= x >> 16; return x; }
vec4 h4(uint x) {
    uint a = hu(x); uint b = hu(a ^ 0x9e3779b9u); uint c = hu(b ^ 0x85ebca6bu); uint d = hu(c ^ 0xc2b2ae35u);
    return vec4(float(a >> 8u), float(b >> 8u), float(c >> 8u), float(d >> 8u)) * (1.0 / 16777216.0);
}
void main() {
    float pxc = uLook.w;
    vec2 css = vec2(gl_FragCoord.x, uBuf.y - gl_FragCoord.y) / pxc;
    vec2 uv = gl_FragCoord.xy / uBuf;
    // A lens scatters light rather than adding it: a share of it leaves the sharp image for the glow.
    vec3 E = (texture(uScene, uv).rgb * (1.0 - uLook.y) + uLook.y * (0.5 * texture(uBloomA, uv).rgb + 0.5 * texture(uBloomB, uv).rgb)
              + texture(uSoft, uv).rgb) * uLook.z;
    vec3 lit = 1.0 - exp(-E * uLook.x);

    vec2 cell = floor(css / 14.0);
    vec4 hs = h4(uint(int(cell.x) + 4096) * 0x8da6b343u + uint(int(cell.y) + 4096) * 0xd8163841u + uint(uSeed) * 0xcb1ab31fu);
    float star = 0.0;
    if (hs.x < uFx.z) {
        vec2 c = (cell + 0.15 + 0.7 * hs.yz) * 14.0;
        vec2 dd = (css - c) * pxc;
        float rad = 0.35 * pxc + 0.2;
        float tw = 0.8 + 0.2 * sin(uFx.w * (0.4 + hs.w) * 2.0 + hs.y * 40.0);
        star = (0.08 + 0.55 * hs.w * hs.w * hs.w) * tw * exp(-dot(dd, dd) / (2.0 * rad * rad));
    }
    lit = 1.0 - (1.0 - lit) * (1.0 - star * uDustE / max(max(uDustE.r, uDustE.g), max(uDustE.b, 1e-3)));

    float la = max(max(lit.r, lit.g), lit.b);
    vec3 rgb = lit + uBg.rgb * uBg.a * (1.0 - lit);
    float al = uBg.a + la * (1.0 - uBg.a);
    float vd = length(uv - 0.5) * 1.4142;
    rgb *= 1.0 - uFx.y * smoothstep(0.35, 1.0, vd);
    vec4 hg = h4(uint(gl_FragCoord.x) * 0x9e3779b1u + uint(gl_FragCoord.y) * 0x85ebca77u + uint(uFx.w * 24.0) * 0xc2b2ae3du);
    rgb += (hg.x - 0.5) * uFx.x * al;
    o = vec4(clamp(rgb, 0.0, al), al);
}
`;

/* ---------- settings ---------- */

const LEVELS = [1, 0.85, 0.72, 0.6]; // resolution steps used by the adaptive quality
const MAX_DPR = 2;
const MAX_PIXELS = 3e6;
const BACKGROUND = '#0a0a0c';

const RESPONSE = { follow: true, reach: 3, swirl: 2, shockwave: 2 };
const PROPS = {
  density: 16,
  pinch: 100,
  fringe: 78,
  speed: 16,
  scale: 288,
  baseColor: '#FF7A45',
  accentColor: '#2DD4BF',
};
const ADV = {
  seed: 1, lean: 100, waist: 22, nearStrands: 50, occupancy: 55, streakLength: 18, streakWidth: 1,
  defocus: 300, coreColor: '#FFE9DC', violetColor: '#1FB5A8', midColor: '#8A8468', hazeColor: '#143C40',
  dustColor: '#D8D0C8', cyanShare: 15, violetShare: 10, exposure: 140, bloom: 75, haze: 60, fibres: 100,
  fieldLight: 16, dust: 4, stars: 0, grain: 0, vignette: 10, flowSpeed: 40, flicker: 12, breathe: 6,
  breathePeriod: 6000, sway: 30, parallax: 30, followTime: 400, returnTime: 650, ringSpeed: 70, sparks: 100,
};
const ANGLE = 180; // the threads run downwards

/* ---------- helpers ---------- */

const clamp = (v, a, b) => Math.min(b, Math.max(a, v));

function hexRgb(hex) {
  const s = hex.replace('#', '');
  return [0, 2, 4].map((i) => parseInt(s.slice(i, i + 2), 16) / 255);
}

/* colour -> light energy (the passes accumulate additively, then tone map) */
function energy(rgb) {
  return rgb.map((c) => -Math.log(1 - 0.97 * Math.min(c, 0.96)));
}

function compileProgram(gl, vsSrc, fsSrc) {
  const shader = (type, src) => {
    const sh = gl.createShader(type);
    if (!sh) throw new Error('createShader failed');
    gl.shaderSource(sh, src);
    gl.compileShader(sh);
    if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS) && !gl.isContextLost()) {
      const log = gl.getShaderInfoLog(sh) || 'shader compile failed';
      gl.deleteShader(sh);
      throw new Error(log);
    }
    return sh;
  };
  const prog = gl.createProgram();
  if (!prog) throw new Error('createProgram failed');
  const vs = shader(gl.VERTEX_SHADER, vsSrc);
  const fs = shader(gl.FRAGMENT_SHADER, fsSrc);
  gl.attachShader(prog, vs);
  gl.attachShader(prog, fs);
  gl.bindAttribLocation(prog, 0, 'aPos');
  gl.linkProgram(prog);
  gl.deleteShader(vs);
  gl.deleteShader(fs);
  if (!gl.getProgramParameter(prog, gl.LINK_STATUS) && !gl.isContextLost()) {
    throw new Error(gl.getProgramInfoLog(prog) || 'link failed');
  }
  return prog;
}

/* a program plus a uniform setter that skips unchanged values */
class Program {
  constructor(gl, prog) {
    this.gl = gl;
    this.prog = prog;
    this.loc = new Map();
    this.last = new Map();
  }
  get(name) {
    let l = this.loc.get(name);
    if (l === undefined) {
      l = this.gl.getUniformLocation(this.prog, name);
      this.loc.set(name, l);
    }
    return l;
  }
  changed(name, a, b = 0, c = 0, d = 0) {
    let last = this.last.get(name);
    if (!last) {
      last = new Float64Array([NaN, NaN, NaN, NaN]);
      this.last.set(name, last);
    }
    if (last[0] === a && last[1] === b && last[2] === c && last[3] === d) return false;
    last[0] = a; last[1] = b; last[2] = c; last[3] = d;
    return true;
  }
  f1(n, a) { if (this.changed(n, a)) this.gl.uniform1f(this.get(n), a); }
  f2(n, a, b) { if (this.changed(n, a, b)) this.gl.uniform2f(this.get(n), a, b); }
  f3(n, a, b, c) { if (this.changed(n, a, b, c)) this.gl.uniform3f(this.get(n), a, b, c); }
  f4(n, a, b, c, d) { if (this.changed(n, a, b, c, d)) this.gl.uniform4f(this.get(n), a, b, c, d); }
  i1(n, a) { if (this.changed(n, a, -1)) this.gl.uniform1i(this.get(n), a); }
  c3(n, v) { this.f3(n, v[0], v[1], v[2]); }
}

/* screen (CSS px in the frame) -> world, the inverse of the shader's toScreen */
function screenToWorld(m, sx, sy) {
  const dx = sx - m.cx;
  const dy = sy - m.cy;
  const st = Math.sin(m.tilt);
  const ry = (dy * m.persp) / Math.max(Math.cos(m.tilt) * m.persp + dy * st, 1);
  const rx = dx / (m.persp / Math.max(m.persp - ry * st, 1));
  const qx = rx * m.ca + ry * m.sa;
  const qy = -rx * m.sa + ry * m.ca;
  return [qx / m.unit, qy / m.unit];
}

/* where thread x0 is drawn at height y (the CPU twin of the shader's threadAt) */
function threadX(eye, x0, y) {
  const dx = x0 - eye.x;
  const u = dx / Math.max(eye.R, 0.001);
  const v = (u * u) / (1.6 * 1.6);
  const P = Math.exp(-v * v);
  const yc = eye.y - eye.lean * Math.tanh(u);
  const g = (y - yc) / Math.max(eye.sig, 0.001);
  const Q = 1 / (1 + g * g);
  const c = eye.a * P * Q;
  return x0 - c * dx + c * (eye.lag * (1 - Q) + eye.tilt * (y - yc));
}

/* which thread ends up at world point (wx, wy): bisection on the monotonic map */
function threadAtPoint(eye, wx, wy) {
  let lo = wx - 3 * eye.R - 1;
  let hi = wx + 3 * eye.R + 1;
  for (let i = 0; i < 40; i++) {
    const mid = 0.5 * (lo + hi);
    if (threadX(eye, mid, wy) < wx) lo = mid; else hi = mid;
  }
  return 0.5 * (lo + hi);
}

const emptyTargets = () => ({ scene: null, soft: null, halfA: [null, null], halfB: [null, null] });

/* ---------- renderer ---------- */

class Threads {
  /** host: element that sizes the canvas; onFail: called once after a fatal error */
  constructor(host, canvas, onFail) {
    this.host = host;
    this.canvas = canvas;
    this.onFail = onFail;

    this.look = this.buildLook();
    this.gl = null;
    this.progs = {};
    this.targets = emptyTargets();
    this.floatOk = false;
    this.useR11 = true;
    this.tris = null; // full-screen triangle
    this.empty = null; // attribute-less vertex array

    this.cssW = 0;
    this.cssH = 0;
    this.scaleX = 1; // layout scale (css transforms)
    this.pxW = 0;
    this.pxH = 0;
    this.pxc = 1; // device px per css px
    this.level = 0;
    this.needsResize = true;

    this.map = { unit: 1, cx: 0, cy: 0, ca: 1, sa: 0, tilt: 0, persp: 1 };
    this.bounds = { x0: -1, x1: 1, y0: -1, y1: 1 };
    this.eye = { x: 0, y: 0, R: 0.26, a: 0.8, sig: 0.15, lean: 0.1, lag: 0, tilt: 0.3 };
    this.vx = 0;
    this.vy = 0;
    this.swirl = 0;
    this.parX = 0;
    this.parY = 0;
    this.firstFrame = true;
    this.moving = false;

    this.time = 0;
    this.flow = 0;
    this.waves = [];
    this.lamps = [];
    this.waveId = 0;

    this.pointer = { x: 0, y: 0, nx: 0.5, ny: 0.5, inside: false, moved: false, down: false, dx: 0, dy: 0, holdUntil: 0 };
    this.world = [0, 0];

    this.reduced = typeof matchMedia === 'function' ? matchMedia('(prefers-reduced-motion: reduce)') : null;
    this.reducedOn = !!(this.reduced && this.reduced.matches);

    this.active = false; // wanted: the screen is not the Mac
    this.inView = true;
    this.docVisible = document.visibilityState !== 'hidden';
    this.lost = false;
    this.dead = false;
    this.running = false;
    this.raf = 0;
    this.dirty = true;
    this.lastT = -1;
    this.lastRender = -1;
    this.frames = 0;
    this.pace = { ratio: 1, slow: 0, fast: 0, hold: 0, ring: [], settle: 0 };

    this.bind();
    this.initGL();
  }

  /* derived look: the colours never change, so work them out once */
  buildLook() {
    const A = ADV;
    const core = energy(hexRgb(A.coreColor));
    const base = energy(hexRgb(PROPS.baseColor));
    const accent = energy(hexRgb(PROPS.accentColor));
    const violet = energy(hexRgb(A.violetColor));
    const mid = energy(hexRgb(A.midColor));
    const haze = energy(hexRgb(A.hazeColor));
    const dust = energy(hexRgb(A.dustColor));
    const fitTo = (c, k) => {
      const s = (k * core[0]) / Math.max(c[0], c[1], c[2], 0.001);
      return [c[0] * s, c[1] * s, c[2] * s];
    };
    const unit = (c) => {
      const m = Math.max(c[0], c[1], c[2], 0.001);
      return [c[0] / m, c[1] / m, c[2] / m];
    };
    const whiten = (c) => c.map((v, i) => 0.25 * v + 0.75 * core[i]);
    const cyan = fitTo(accent, 1);
    const vio = fitTo(violet, 1);
    const hazeDir = unit(haze);
    const coreDir = unit(core);
    const tint = hazeDir.map((v, i) => 0.3 + 0.7 * Math.min(1.5, v / Math.max(coreDir[i], 0.05)));
    return {
      core, cyan, vio, mid, haze, dust, hazeDir, tint,
      warm: fitTo(base, 0.8),
      cyanSharp: whiten(cyan),
      vioSharp: whiten(vio),
      bg: hexRgb(BACKGROUND),
    };
  }

  /* ----- GL setup ----- */
  initGL() {
    const gl = this.canvas.getContext('webgl2', {
      alpha: true, antialias: false, depth: false, stencil: false,
      premultipliedAlpha: true, preserveDrawingBuffer: false, powerPreference: 'low-power',
    });
    if (!gl) { this.fail(new Error('webgl2 unavailable')); return; }
    this.gl = gl;
    try {
      const mk = (vs, fs) => new Program(gl, compileProgram(gl, vs, fs));
      this.progs = {
        thread: mk(THREAD_VS, THREAD_FS),
        glint: mk(MAGPIE_VS, MAGPIE_FS),
        dust: mk(DUST_VS, POINT_FS),
        spark: mk(SPARK_VS, POINT_FS),
        haze: mk(FULL_VS, HAZE_FS),
        down: mk(FULL_VS, DOWN_FS),
        blur: mk(FULL_VS, BLUR_FS),
        comp: mk(FULL_VS, COMPOSITE_FS),
      };
    } catch (err) {
      if (!gl.isContextLost()) this.fail(err);
      return;
    }
    this.floatOk = !!gl.getExtension('EXT_color_buffer_float') || !!gl.getExtension('EXT_color_buffer_half_float');
    gl.getExtension('OES_texture_float_linear');
    this.tq = gl.getExtension('EXT_disjoint_timer_query_webgl2');
    this.tqPend = null;
    this.gpuMs = -1;

    this.buf = gl.createBuffer();
    this.tris = gl.createVertexArray();
    gl.bindVertexArray(this.tris);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    this.empty = gl.createVertexArray();
    gl.bindVertexArray(null);

    this.useR11 = true;
    this.targets = emptyTargets();
    this.pxW = this.pxH = 0;
    this.needsResize = true;
  }

  makeTarget(w, h, hdr) {
    const gl = this.gl;
    const tex = gl.createTexture();
    const fbo = gl.createFramebuffer();
    if (!tex || !fbo) return null;
    gl.bindTexture(gl.TEXTURE_2D, tex);
    if (hdr && this.useR11) {
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.R11F_G11F_B10F, w, h, 0, gl.RGB, gl.HALF_FLOAT, null);
    } else {
      gl.texImage2D(gl.TEXTURE_2D, 0, hdr ? gl.RGBA16F : gl.RGBA8, w, h, 0, gl.RGBA, hdr ? gl.HALF_FLOAT : gl.UNSIGNED_BYTE, null);
    }
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.bindFramebuffer(gl.FRAMEBUFFER, fbo);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0);
    const ok = gl.checkFramebufferStatus(gl.FRAMEBUFFER) === gl.FRAMEBUFFER_COMPLETE;
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    if (ok) return { tex, fbo, w, h };
    gl.deleteTexture(tex);
    gl.deleteFramebuffer(fbo);
    if (hdr && this.useR11) {
      this.useR11 = false; // retry as RGBA16F
      return this.makeTarget(w, h, hdr);
    }
    return null;
  }

  freeTarget(t) {
    if (t && this.gl) {
      this.gl.deleteTexture(t.tex);
      this.gl.deleteFramebuffer(t.fbo);
    }
  }

  allTargets() {
    const t = this.targets;
    return [t.scene, t.soft, t.halfA[0], t.halfA[1], t.halfB[0], t.halfB[1]];
  }

  /* resolution: DPR (max 2) x layout scale x quality step, capped by total pixels */
  resize() {
    this.needsResize = false;
    if (!this.gl || this.cssW <= 0 || this.cssH <= 0) return;
    let res = Math.min(window.devicePixelRatio || 1, MAX_DPR) * this.scaleX * LEVELS[this.level];
    const area = this.cssW * this.cssH * res * res;
    if (area > MAX_PIXELS) res *= Math.sqrt(MAX_PIXELS / area);
    const w = Math.max(1, Math.round(this.cssW * res));
    const h = Math.max(1, Math.round(this.cssH * res));
    this.pxc = w / this.cssW;
    if (w === this.pxW && h === this.pxH && this.targets.scene) return;
    this.pxW = w;
    this.pxH = h;
    this.canvas.width = w;
    this.canvas.height = h;
    this.allTargets().forEach((t) => this.freeTarget(t));
    let hdr = this.floatOk;
    let scene = this.makeTarget(w, h, hdr);
    if (!scene && hdr) {
      this.floatOk = hdr = false;
      scene = this.makeTarget(w, h, false);
    }
    const qw = Math.max(1, Math.ceil(w / 4));
    const qh = Math.max(1, Math.ceil(h / 4));
    const ew = Math.max(1, Math.ceil(w / 8));
    const eh = Math.max(1, Math.ceil(h / 8));
    this.targets = {
      scene,
      soft: this.makeTarget(qw, qh, hdr),
      halfA: [this.makeTarget(qw, qh, hdr), this.makeTarget(qw, qh, hdr)],
      halfB: [this.makeTarget(ew, eh, hdr), this.makeTarget(ew, eh, hdr)],
    };
    this.dirty = true;
  }

  /* ----- events ----- */
  bind() {
    const p = this.pointer;
    const inRect = (e) => {
      const r = this.host.getBoundingClientRect();
      return r.width > 0 && e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom;
    };
    const place = (e) => {
      p.x = e.clientX;
      p.y = e.clientY;
      p.moved = true;
    };
    this.on = {
      move: (e) => {
        if (!this.active) return;
        if (!inRect(e)) {
          if (p.inside) { p.inside = false; this.wake(); }
          return;
        }
        if (e.clientX === p.x && e.clientY === p.y && p.inside) return;
        place(e);
        p.inside = true;
        this.wake();
      },
      leave: (e) => {
        const touch = e && e.pointerType && e.pointerType !== 'mouse';
        // a finger lifted: keep following the last touch for a moment
        if (touch) {
          p.holdUntil = performance.now() + 4000;
          setTimeout(() => this.wake(), 4050);
        }
        p.inside = false;
        this.wake();
      },
      down: (e) => {
        if (!this.active || !inRect(e)) return;
        if (e.pointerType === 'mouse' && e.button !== 0) return;
        if (e.target && e.target.closest && e.target.closest('a, button, input, textarea, select, [role="button"], .crt-screen')) return;
        place(e);
        p.inside = true;
        p.down = true;
        p.dx = e.clientX;
        p.dy = e.clientY;
        this.wake();
      },
      up: (e) => { if (e.pointerType !== 'mouse') this.on.leave(e); },
      scroll: () => { if (p.inside) { p.moved = true; this.wake(); } },
      vis: () => { this.docVisible = document.visibilityState !== 'hidden'; this.syncRun(); },
      lamps: () => { if (!this.running) this.wake(); },
      motion: () => { this.reducedOn = !!(this.reduced && this.reduced.matches); this.dirty = true; this.wake(); },
      lost: (e) => {
        e.preventDefault();
        this.lost = true;
        this.stop();
      },
      restored: () => {
        this.lost = false;
        this.progs = {};
        this.initGL();
        this.dirty = true;
        this.syncRun();
      },
    };
    window.addEventListener('pointermove', this.on.move, { passive: true });
    window.addEventListener('pointerdown', this.on.down, { passive: true });
    window.addEventListener('pointerup', this.on.up, { passive: true });
    window.addEventListener('pointercancel', this.on.leave, { passive: true });
    document.documentElement.addEventListener('pointerleave', this.on.leave);
    window.addEventListener('scroll', this.on.scroll, { passive: true, capture: true });
    window.addEventListener('lab:lamps', this.on.lamps);
    document.addEventListener('visibilitychange', this.on.vis);
    if (this.reduced && this.reduced.addEventListener) this.reduced.addEventListener('change', this.on.motion);
    this.canvas.addEventListener('webglcontextlost', this.on.lost);
    this.canvas.addEventListener('webglcontextrestored', this.on.restored);

    this.ro = new ResizeObserver((entries) => {
      const r = entries[entries.length - 1].contentRect;
      this.cssW = r.width;
      this.cssH = r.height;
      this.measureScale();
      this.needsResize = true;
      this.dirty = true;
      this.wake();
    });
    this.ro.observe(this.host);
    if (typeof IntersectionObserver === 'function') {
      this.io = new IntersectionObserver((entries) => {
        this.inView = entries[entries.length - 1].isIntersecting;
        this.syncRun();
      });
      this.io.observe(this.host);
    }
    const r = this.host.getBoundingClientRect();
    this.cssW = this.host.clientWidth || r.width;
    this.cssH = this.host.clientHeight || r.height;
    this.measureScale();
  }

  measureScale() {
    if (this.cssW <= 0) return;
    const s = clamp(this.host.getBoundingClientRect().width / this.cssW, 1, 2);
    if (Math.abs(s - this.scaleX) / this.scaleX > 0.05) {
      this.scaleX = s;
      this.needsResize = true;
    }
  }

  /* ----- run control ----- */
  setActive(on) {
    this.active = on;
    if (!on) {
      this.pointer.inside = false;
      this.pointer.down = false;
    }
    this.syncRun();
  }

  get canRun() {
    return this.active && this.inView && this.docVisible && !this.lost && !this.dead && !!this.gl;
  }

  syncRun() {
    if (this.canRun) this.wake(); else this.stop();
  }

  wake() {
    this.dirty = true;
    if (this.running || !this.canRun) return;
    this.running = true;
    this.lastT = -1;
    this.lastRender = -1;
    this.raf = requestAnimationFrame((t) => this.frame(t));
  }

  stop() {
    this.running = false;
    if (this.raf) cancelAnimationFrame(this.raf);
    this.raf = 0;
  }

  /* target frame rate: 60 while anything reacts, 30 for the idle flow, 0 when still */
  targetFps() {
    if (this.moving || this.waves.length) return 60;
    const speed = PROPS.speed / 50;
    if (this.reducedOn || speed <= 0) return 0;
    return (ADV.flowSpeed / 100) * speed * this.map.unit > 45 ? 60 : 30;
  }

  frame(t) {
    this.raf = 0;
    if (!this.running) return;
    if (!this.canRun) { this.running = false; return; }
    try {
      const dt = this.lastT < 0 ? 0 : t - this.lastT;
      this.lastT = t;
      const seconds = Math.min(0.1, dt / 1000);

      this.advanceClock(seconds);
      this.readPointer();
      this.updateMap();
      this.world = screenToWorld(this.map, this.pointer.nx * this.cssW, this.pointer.ny * this.cssH);
      this.updateEye(seconds, t);
      this.waves.forEach((w) => { w.age += seconds; });
      while (this.waves.length && this.waves[0].age > 1.9) this.waves.shift();

      const fps = this.targetFps();
      const interval = fps > 0 ? 1000 / fps - 2 : 0;
      const due = this.lastRender < 0 || t - this.lastRender >= interval;
      if (this.dirty || (fps > 0 && due)) {
        const since = this.lastRender < 0 ? 0 : t - this.lastRender;
        this.lastRender = t;
        this.dirty = false;
        this.timedDraw();
        this.frames++;
        this.adapt(fps > 0 ? 1000 / fps : 0, since);
        if (this.frames % 30 === 0) this.measureScale();
      }
      if (this.dead) return;
      if (fps > 0 || this.dirty) this.raf = requestAnimationFrame((n) => this.frame(n));
      else { this.running = false; this.lastT = -1; }
    } catch (err) {
      this.fail(err);
    }
  }

  /* draw, and time the GPU side when the browser allows it so quality follows GPU load only */
  timedDraw() {
    const gl = this.gl;
    const tq = this.tq;
    if (!tq) return this.draw();
    if (this.tqPend) {
      const q = this.tqPend;
      if (gl.getQueryParameter(q, gl.QUERY_RESULT_AVAILABLE)) {
        if (!gl.getParameter(tq.GPU_DISJOINT_EXT)) this.gpuMs = gl.getQueryParameter(q, gl.QUERY_RESULT) / 1e6;
        else this.gpuMs = -1;
        gl.deleteQuery(q);
        this.tqPend = null;
      }
    }
    if (this.tqPend) return this.draw();
    const q = gl.createQuery();
    gl.beginQuery(tq.TIME_ELAPSED_EXT, q);
    this.draw();
    gl.endQuery(tq.TIME_ELAPSED_EXT);
    this.tqPend = q;
  }

  advanceClock(dt) {
    if (this.reducedOn) return;
    const speed = PROPS.speed / 50;
    this.time = (this.time + dt * speed) % 36000;
    this.flow = (this.flow + dt * speed * (ADV.flowSpeed / 100)) % 3600;
  }

  /* pointer position inside the frame, and a new shock ring when clicked */
  readPointer() {
    const p = this.pointer;
    if (!p.moved && !p.down) return;
    p.moved = false;
    const r = this.host.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) return;
    p.nx = (p.x - r.left) / r.width;
    p.ny = (p.y - r.top) / r.height;
    this.measureScale();
    if (!p.down) return;
    p.down = false;
    const strength = clamp(RESPONSE.shockwave, 0, 10) / 5;
    if (strength <= 0 || this.reducedOn) return;
    this.updateMap();
    const [wx, wy] = screenToWorld(this.map, ((p.dx - r.left) / r.width) * this.cssW, ((p.dy - r.top) / r.height) * this.cssH);
    if (this.waves.length >= 4) this.waves.shift();
    this.waves.push({ x: wx, y: wy, x0: threadAtPoint(this.eye, wx, wy), age: 0, amp: Math.min(2, strength), id: this.waveId++ });
  }

  /* the room's lamps (world.js), brightest six, as world-space field lights */
  readLamps() {
    const out = this.lamps;
    out.length = 0;
    const src = window.__world && window.__world.lamps;
    const list = src ? src() : null;
    if (!list || !list.length) return;
    const r = this.host.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) return;
    const k = this.cssW / r.width;
    list.sort((a, b) => b.b - a.b);
    for (const l of list) {
      if (out.length >= 6) break;
      if (l.x < r.left - l.r || l.x > r.right + l.r || l.y < r.top - l.r || l.y > r.bottom + l.r) continue;
      const [wx, wy] = screenToWorld(this.map, (l.x - r.left) * k, (l.y - r.top) * k);
      const reach = (0.55 * l.r * k) / this.map.unit;
      out.push([wx, wy, 1 / (reach * reach), 0.5 * l.b]);
    }
  }

  updateMap() {
    const m = this.map;
    m.unit = Math.min(this.cssH, this.cssW / 1.5) * (clamp(PROPS.scale, 30, 300) / 100);
    m.cx = this.cssW / 2;
    m.cy = this.cssH / 2;
    const a = (ANGLE * Math.PI) / 180;
    m.ca = Math.cos(a);
    m.sa = Math.sin(a);
    m.tilt = 0;
    m.persp = 1.5 * Math.max(this.cssW, this.cssH, 1);
  }

  /* the eye of the bundle chases the pointer on a critically damped spring */
  updateEye(dt, now) {
    const A = ADV;
    const eye = this.eye;
    const p = this.pointer;
    eye.R = 0.08 + 0.052 * clamp(RESPONSE.reach, 0, 10);
    const pinch = clamp(PROPS.pinch, 0, 100) / 100;
    const breathe = (A.breathe / 100) * Math.sin((2 * Math.PI * this.time) / (A.breathePeriod / 1000));
    eye.a = clamp((0.2 + 0.75 * Math.pow(pinch, 0.75)) * (1 + breathe), 0, 0.97);
    eye.sig = (A.waist / 100) * (1 - 0.5 * breathe);

    // at rest the eye wanders slowly on its own
    const idleX = 0.018 * Math.sin(0.37 * this.time) + 0.008 * Math.sin(0.91 * this.time + 2);
    const idleY = 0.012 * Math.sin(0.23 * this.time + 1);
    const follow = RESPONSE.follow !== false && !this.reducedOn && (p.inside || now < p.holdUntil);
    const tx = follow ? this.world[0] : idleX;
    const ty = follow ? this.world[1] : idleY;
    if (this.firstFrame) {
      this.firstFrame = false;
      eye.x = tx;
      eye.y = ty;
      this.vx = this.vy = 0;
    }
    const k = 5.8 / (follow ? A.followTime / 1000 : A.returnTime / 1000);
    for (let left = dt; left > 1e-6;) {
      const h = Math.min(left, 1 / 240);
      this.vx += (k * k * (tx - eye.x) - 2 * k * this.vx) * h;
      this.vy += (k * k * (ty - eye.y) - 2 * k * this.vy) * h;
      eye.x += this.vx * h;
      eye.y += this.vy * h;
      left -= h;
    }
    if (Math.abs(tx - eye.x) < 1e-5 && Math.abs(this.vx) < 1e-4) { eye.x = tx; this.vx = 0; }
    if (Math.abs(ty - eye.y) < 1e-5 && Math.abs(this.vy) < 1e-4) { eye.y = ty; this.vy = 0; }

    // swirl: fast sideways sweeps twist the threads
    const swirlGain = clamp(RESPONSE.swirl, 0, 10) / 5;
    const target = clamp(0.9 * this.vx, -1.5, 1.5) * swirlGain;
    this.swirl += (target - this.swirl) * (1 - Math.exp(-dt / 0.22));
    if (Math.abs(target - this.swirl) < 1e-4) this.swirl = target;

    const lean = A.lean / 100;
    eye.lean = eye.R * (0.6 * lean + 0.35 * this.swirl);
    eye.lag = -0.16 * this.swirl;
    eye.tilt = 0.45 * lean + 0.25 * this.swirl;
    const par = A.parallax / 100;
    this.parX = -0.12 * par * eye.x;
    this.parY = -0.05 * par * eye.y;
    this.moving = eye.x !== tx || eye.y !== ty || this.vx !== 0 || this.vy !== 0 || Math.abs(this.swirl) > 1e-4;
  }

  /* drop to a cheaper resolution step when frames run long, climb back when there is room */
  adapt(target, since) {
    const q = this.pace;
    if (target <= 0 || this.frames < 30 || since <= 0) return;
    const now = performance.now();
    // main-thread hitches (page load, other scripts, tab switches) are not GPU load: ignore long gaps
    // and judge quality on the median of recent frames, and never during the first seconds of the page
    if (!q.settle) q.settle = now + 6000;
    if (now < q.settle) return;
    const ring = q.ring;
    const budget = Math.min(target, 33.4);
    if (this.tq) {
      if (this.gpuMs < 0) return;
      ring.push(this.gpuMs / (0.7 * budget));
      this.gpuMs = -1;
    } else {
      if (since > 100 || document.readyState !== 'complete') { q.slow = 0; return; }
      ring.push(since / budget);
    }
    if (ring.length > 15) ring.shift();
    if (ring.length < 9) return;
    q.ratio = [...ring].sort((a, b) => a - b)[ring.length >> 1];
    const secs = since / 1000;
    if (q.ratio > 1.3) { q.slow += secs; q.fast = 0; } else { q.slow = 0; q.fast += secs; }
    if (q.slow > 0.5 && this.level < LEVELS.length - 1) {
      this.level++;
      q.slow = q.fast = 0;
      ring.length = 0;
      q.hold = now + 4000;
      this.needsResize = true;
    } else if (q.fast > 1.5 && this.level > 0 && now > q.hold) {
      this.level--;
      q.fast = 0;
      ring.length = 0;
      q.hold = now + 4000;
      this.needsResize = true;
    }
  }

  /* ----- drawing ----- */
  setCommon(u, phase) {
    const A = ADV;
    const m = this.map;
    const eye = this.eye;
    u.f4('uMap', m.ca, m.sa, m.tilt, m.persp);
    u.f4('uMap2', m.unit, m.cx, m.cy, this.pxc);
    u.f2('uFrame', this.cssW, this.cssH);
    u.f4('uEye', eye.x, eye.y, eye.R, eye.a);
    u.f4('uEye2', eye.sig, eye.lean, eye.lag, eye.y);
    u.f1('uTilt', eye.tilt);
    u.f4('uDof', 0.17, 0.011 * (clamp(A.defocus, 0, 300) / 100), 0.45 * clamp(A.streakWidth, 1, 20), 0.8 / this.pxc);
    u.f4('uSway', (clamp(A.sway, 0, 100) / 100) * 0.006, 0.35 * phase, this.parX, this.parY);
    const ringSpeed = (clamp(A.ringSpeed, 10, 200) / 100) * 1.1;
    for (let i = 0; i < 4; i++) {
      const w = this.waves[i];
      const fade = w ? Math.max(0, 1 - w.age / 1.9) : 0;
      if (w && fade > 0) u.f4(`uWave[${i}]`, w.x, w.y, ringSpeed * w.age, w.amp * fade * fade);
      else u.f4(`uWave[${i}]`, 0, 0, 0, 0);
    }
    for (let i = 0; i < 6; i++) {
      const l = this.lamps[i];
      if (l) u.f4(`uLamp[${i}]`, l[0], l[1], l[2], l[3]);
      else u.f4(`uLamp[${i}]`, 0, 0, 0, 0);
    }
    u.f4('uWaveP', 0.032, 0.034 * Math.max(clamp(RESPONSE.shockwave, 0, 10) / 5, 0.2), 4, 0);
    u.f1('uSalt', Math.round(clamp(A.seed, 0, 99)));
    u.f1('uBundleShift', (0.02 * this.time) % 1000);
  }

  /* one full-screen pass into a target */
  pass(target, prog) {
    const gl = this.gl;
    if (!target) return;
    gl.bindFramebuffer(gl.FRAMEBUFFER, target.fbo);
    gl.viewport(0, 0, target.w, target.h);
    gl.useProgram(prog.prog);
    gl.bindVertexArray(this.tris);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }

  draw() {
    if (this.needsResize) this.resize();
    const gl = this.gl;
    const P = this.progs;
    const T = this.targets;
    if (!gl || this.dead || !T.scene || !P.comp || this.cssW <= 0 || this.cssH <= 0) return;

    const A = ADV;
    const look = this.look;
    const m = this.map;
    const eye = this.eye;
    this.updateMap();
    this.updateBounds();
    this.readLamps();
    const b = this.bounds;

    const density = clamp(PROPS.density, 10, 200) / 100;
    const phase = (2 * Math.PI * this.time) / (clamp(A.breathePeriod, 500, 30000) / 1000);
    const out = this.floatOk ? 1 : 0.25; // 8-bit fallback needs the headroom
    const fringe = clamp(PROPS.fringe, 0, 100) / 100;
    const fieldLight = clamp(A.fieldLight, 0, 100) / 100;
    const fibres = clamp(A.fibres, 0, 300) / 100;
    const flicker = clamp(A.flicker, 0, 100) / 100;

    gl.bindFramebuffer(gl.FRAMEBUFFER, T.scene.fbo);
    gl.viewport(0, 0, T.scene.w, T.scene.h);
    gl.clearColor(0, 0, 0, 0);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE);
    gl.bindVertexArray(this.empty);

    const margin = 0.9 * eye.R + 0.03 + Math.abs(this.parX) + 0.02;

    /* 1. the thread sheet */
    const th = P.thread;
    const pitch = 1 / (380 * density);
    let stride = 1;
    while (pitch * stride * m.unit < 2.2 && stride < 64) stride *= 2;
    while ((b.x1 - b.x0 + 2 * margin) / (pitch * stride) > 2600 && stride < 256) stride *= 2;
    const first = Math.floor((b.x0 - margin) / pitch / stride) * stride;
    const last = Math.ceil((b.x1 + margin) / pitch / stride) * stride;
    const count = Math.max(0, Math.round((last - first) / stride) + 1);
    const drawThreads = (layer, step, start, str, n, gain, gather, seg) => {
      if (n <= 0) return;
      th.f4('uTh', start, str, step, gain);
      th.f4('uTh2', b.y0 - 0.04, b.y1 + 0.04, gather, layer);
      th.f1('uSeg', seg);
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, n * (2 * (seg + 1) + 2));
    };
    gl.useProgram(th.prog);
    this.setCommon(th, phase);
    th.f4('uTh3', this.flow, fibres, 0.55 * fibres, fieldLight);
    th.f4('uTh4', 0.12 * fringe, 0.45 * fringe, 9, 0.25);
    th.f4('uTh5', 1, this.time, flicker, 0);
    th.c3('uK0', look.core);
    th.c3('uK1', look.warm);
    th.c3('uK2', look.cyan);
    th.c3('uK3', look.vio);
    th.c3('uK2s', look.cyanSharp);
    th.c3('uK3s', look.vioSharp);
    th.c3('uMidE', look.mid);
    th.c3('uTint', look.tint);
    th.f1('uOut', out);
    const gain = 4.2 * Math.pow(1 / density, 0.5) * stride;
    if (count > 0) {
      // more segments only where the threads bend, around the eye
      const heightPx = (b.y1 - b.y0 + 0.08) * m.unit;
      const segHi = clamp(Math.ceil(heightPx / 20), 24, 96);
      const segLo = this.waves.length ? segHi : clamp(Math.ceil(heightPx / 34), 16, 96);
      const reach = 1.6 * eye.R + 0.02;
      const a = Math.min(last + stride, Math.max(first, Math.floor((eye.x - reach) / pitch / stride) * stride));
      const c = Math.max(a - stride, Math.min(last, Math.ceil((eye.x + reach) / pitch / stride) * stride));
      drawThreads(0, pitch, first, stride, Math.round((a - first) / stride), gain, 1, segLo);
      drawThreads(0, pitch, a, stride, Math.round((c - a) / stride) + 1, gain, 1, segHi);
      drawThreads(0, pitch, c + stride, stride, Math.round((last - c) / stride), gain, 1, segLo);
    }

    /* 2. glints: short bright spindles drifting up the threads */
    const gp = 1 / (105 * density);
    let gs = 1;
    while (gp * gs * m.unit < 6 && gs < 64) gs *= 2;
    const gFirst = Math.floor((b.x0 - margin) / gp / gs) * gs;
    const gLanes = Math.max(0, Math.min(1200, Math.round((Math.ceil((b.x1 + margin) / gp / gs) * gs - gFirst) / gs) + 1));
    const rangeStart = 0.25 * Math.floor((b.y0 - 0.4) / 0.25);
    const rangeLen = Math.max(0.5, 0.25 * Math.ceil((b.y1 + 0.4) / 0.25) - rangeStart);
    const streak = 1.4 * Math.max(0.03, clamp(A.streakLength, 5, 60) / 100);
    const slots = clamp(Math.ceil(rangeLen / streak), 1, 48);
    if (gLanes > 0) {
      const g = P.glint;
      gl.useProgram(g.prog);
      this.setCommon(g, phase);
      g.f4('uGl', gFirst, gLanes, slots, gp);
      g.f4('uGl2', rangeStart, rangeLen, this.flow, clamp(A.occupancy, 5, 100) / 100);
      g.f4('uGl3', 0.45 * clamp(A.streakWidth, 1, 20), 4 * Math.pow(1 / density, 0.5), clamp(A.cyanShare, 0, 100) / 100, clamp(A.violetShare, 0, 100) / 100);
      g.f4('uGl4', flicker, this.time, 0.45 * fringe, 0.6 * fringe);
      g.f4('uGl5', fieldLight, streak / 0.25, gs, 0);
      g.c3('uCoreE', look.core);
      g.c3('uBaseE', look.warm);
      g.c3('uAccE', look.cyan);
      g.c3('uVioE', look.vio);
      g.f1('uOut', out);
      gl.drawArrays(gl.TRIANGLES, 0, gLanes * slots * 60);
    }

    /* 3. dust */
    const dustAmt = clamp(A.dust, 0, 200) / 50;
    if (dustAmt > 0) {
      const d = P.dust;
      const sizeK = Math.min(3, Math.max(0.05, (m.unit / 800) ** 2));
      const area = (b.x1 - b.x0 + 2 * margin) * rangeLen;
      const cluster = Math.round(16000 * (eye.R / 0.26) * Math.min(2, dustAmt) * sizeK);
      const total = Math.min(2e5, cluster + Math.round(7000 * area * Math.min(2, dustAmt) * sizeK));
      gl.useProgram(d.prog);
      this.setCommon(d, phase);
      d.f4('uDu', rangeStart, rangeLen, this.flow, Math.max(1, 0.75 * this.pxc));
      d.f4('uDu2', cluster, b.x0 - margin, b.x1 - b.x0 + 2 * margin, 1.1 * Math.max(1, dustAmt / 2));
      d.f4('uDu3', this.time, fieldLight, 0, 0);
      d.c3('uDustE', look.dust);
      d.f1('uOut', out);
      if (total > 0) gl.drawArrays(gl.POINTS, 0, total);
    }

    /* 4. click sparks */
    const sparkAmt = clamp(A.sparks, 0, 300) / 100;
    if (sparkAmt > 0 && this.waves.length) {
      const s = P.spark;
      gl.useProgram(s.prog);
      this.setCommon(s, phase);
      s.c3('uHot', look.core.map((v) => 3 * v));
      s.c3('uWarm', look.warm.map((v) => 1.5 * v));
      s.f1('uOut', out);
      const n = Math.round(360 * Math.min(3, sparkAmt));
      for (const w of this.waves) {
        if (w.age > 1.7) continue;
        s.f4('uSp', w.x0, w.y, w.age, 5 * w.amp);
        s.f4('uSp2', Math.max(1.5, 2.1 * this.pxc), w.id % 997, this.flow, 0);
        gl.drawArrays(gl.POINTS, 0, n);
      }
    }
    gl.disable(gl.BLEND);

    /* 5. soft layer (haze, shock glow, near strands) at quarter resolution */
    if (T.soft) {
      const hz = P.haze;
      const softPxc = (T.soft.w / this.pxW) * this.pxc;
      gl.bindFramebuffer(gl.FRAMEBUFFER, T.soft.fbo);
      gl.viewport(0, 0, T.soft.w, T.soft.h);
      gl.useProgram(hz.prog);
      this.setCommon(hz, phase);
      hz.f2('uBuf', T.soft.w, T.soft.h);
      hz.f1('uPxc', softPxc);
      const hazeAmt = (clamp(A.haze, 0, 200) / 100) * 0.6;
      hz.f4('uHazeE', look.hazeDir[0], look.hazeDir[1], look.hazeDir[2], Math.max(...look.haze) * hazeAmt);
      const ring = ((0.12 * clamp(RESPONSE.shockwave, 0, 10)) / 5) * 0.6;
      hz.f3('uRingE', (look.warm[0] + look.core[0]) * ring, (look.warm[1] + look.core[1]) * ring, (look.warm[2] + look.core[2]) * ring);
      hz.f1('uOut', out);
      gl.bindVertexArray(this.tris);
      gl.drawArrays(gl.TRIANGLES, 0, 3);

      const near = clamp(A.nearStrands, 0, 200) / 100;
      if (near > 0) {
        gl.enable(gl.BLEND);
        gl.blendFunc(gl.ONE, gl.ONE);
        gl.bindVertexArray(this.empty);
        gl.useProgram(th.prog);
        const step = 1 / 12;
        const n0 = Math.floor((b.x0 - margin - 2 * Math.abs(this.parX)) / step);
        const n1 = Math.ceil((b.x1 + margin + 2 * Math.abs(this.parX)) / step);
        th.f4('uMap2', m.unit, m.cx, m.cy, softPxc);
        th.f4('uTh3', 0.6 * this.flow, fibres, 0.55 * fibres, 0.55);
        th.f4('uTh4', 1.6 * fringe, 0.35 * fringe, 16, 0.35 * near);
        drawThreads(1, step, n0, 1, Math.max(0, Math.min(400, n1 - n0 + 1)), 1, 0.25, 96);
        gl.disable(gl.BLEND);
      }
    }

    /* 6. bloom: two blurred, downsampled copies of the scene */
    const bloom = clamp(A.bloom, 0, 200) / 100;
    const [qa, qb] = T.halfA;
    const [ea, eb] = T.halfB;
    if (bloom > 0 && qa && qb && ea && eb) {
      const dn = P.down;
      const bl = P.blur;
      gl.activeTexture(gl.TEXTURE0);
      gl.useProgram(dn.prog);
      dn.i1('uSrc', 0);
      gl.bindTexture(gl.TEXTURE_2D, T.scene.tex);
      dn.f2('uSrcPx', 1 / T.scene.w, 1 / T.scene.h);
      dn.f2('uDstRes', qa.w, qa.h);
      this.pass(qa, dn);
      gl.bindTexture(gl.TEXTURE_2D, qa.tex);
      dn.f2('uSrcPx', 1 / qa.w, 1 / qa.h);
      dn.f2('uDstRes', ea.w, ea.h);
      this.pass(ea, dn);
      gl.useProgram(bl.prog);
      bl.i1('uSrc', 0);
      for (const [pair, sx, sy] of [[T.halfA, 1 / qa.w, 1 / qa.h], [T.halfB, 1 / ea.w, 1 / ea.h]]) {
        const [r, o] = pair;
        bl.f2('uDstRes', r.w, r.h);
        gl.bindTexture(gl.TEXTURE_2D, r.tex);
        bl.f2('uStep', sx, 0);
        this.pass(o, bl);
        gl.bindTexture(gl.TEXTURE_2D, o.tex);
        bl.f2('uStep', 0, sy);
        this.pass(r, bl);
      }
    }

    /* 7. composite onto the canvas */
    const cp = P.comp;
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.viewport(0, 0, this.pxW, this.pxH);
    gl.useProgram(cp.prog);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, T.scene.tex);
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, qa ? qa.tex : T.scene.tex);
    gl.activeTexture(gl.TEXTURE2);
    gl.bindTexture(gl.TEXTURE_2D, ea ? ea.tex : T.scene.tex);
    gl.activeTexture(gl.TEXTURE3);
    gl.bindTexture(gl.TEXTURE_2D, T.soft ? T.soft.tex : T.scene.tex);
    cp.i1('uScene', 0);
    cp.i1('uBloomA', 1);
    cp.i1('uBloomB', 2);
    cp.i1('uSoft', 3);
    cp.f2('uBuf', this.pxW, this.pxH);
    cp.f4('uLook', clamp(A.exposure, 20, 400) / 140, Math.min(0.9, 0.6 * bloom), 1 / out, this.pxc);
    cp.f4('uBg', look.bg[0], look.bg[1], look.bg[2], 1);
    cp.f4('uFx', (clamp(A.grain, 0, 100) / 100) * 0.12, clamp(A.vignette, 0, 100) / 100, 0.06 * (clamp(A.stars, 0, 200) / 50), this.time);
    cp.c3('uDustE', look.dust);
    cp.f1('uSeed', Math.round(clamp(A.seed, 0, 99)));
    gl.bindVertexArray(this.tris);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    gl.bindVertexArray(null);
  }

  /* world-space rectangle covered by the frame */
  updateBounds() {
    let x0 = Infinity, x1 = -Infinity, y0 = Infinity, y1 = -Infinity;
    const w = this.cssW;
    const h = this.cssH;
    for (const [sx, sy] of [[0, 0], [w, 0], [0, h], [w, h], [w / 2, 0], [w / 2, h]]) {
      const [wx, wy] = screenToWorld(this.map, sx, sy);
      x0 = Math.min(x0, wx); x1 = Math.max(x1, wx);
      y0 = Math.min(y0, wy); y1 = Math.max(y1, wy);
    }
    const b = this.bounds;
    b.x0 = Math.max(-8, x0); b.x1 = Math.min(8, x1);
    b.y0 = Math.max(-8, y0); b.y1 = Math.min(8, y1);
  }

  /* ----- teardown ----- */
  release() {
    const gl = this.gl;
    if (gl && !gl.isContextLost()) {
      this.allTargets().forEach((t) => this.freeTarget(t));
      Object.values(this.progs).forEach((p) => p && gl.deleteProgram(p.prog));
      if (this.buf) gl.deleteBuffer(this.buf);
      if (this.tris) gl.deleteVertexArray(this.tris);
      if (this.empty) gl.deleteVertexArray(this.empty);
      const lose = gl.getExtension('WEBGL_lose_context');
      if (lose) lose.loseContext();
    }
    this.targets = emptyTargets();
    this.progs = {};
    this.gl = null;
  }

  unbind() {
    window.removeEventListener('pointermove', this.on.move);
    window.removeEventListener('pointerdown', this.on.down);
    window.removeEventListener('pointerup', this.on.up);
    window.removeEventListener('pointercancel', this.on.leave);
    document.documentElement.removeEventListener('pointerleave', this.on.leave);
    window.removeEventListener('scroll', this.on.scroll, { capture: true });
    window.removeEventListener('lab:lamps', this.on.lamps);
    document.removeEventListener('visibilitychange', this.on.vis);
    if (this.reduced && this.reduced.removeEventListener) this.reduced.removeEventListener('change', this.on.motion);
    this.canvas.removeEventListener('webglcontextlost', this.on.lost);
    this.canvas.removeEventListener('webglcontextrestored', this.on.restored);
    this.ro.disconnect();
    if (this.io) this.io.disconnect();
  }

  /* fatal: stop, free everything and tell the owner to drop the layer */
  fail(err) {
    if (this.dead) return;
    this.dead = true;
    this.stop();
    console.warn('backdrop: threads disabled:', err && err.message ? err.message : err);
    this.unbind();
    this.release();
    this.onFail();
  }
}

/* ---------- layer in the desk stage ---------- */

const stage = document.getElementById('film');
const FADE_MS = 520;
let layer = null;
let threads = null;
let failed = false;
let shown = false;
let sleepTimer = 0;

function ensureLayer() {
  if (layer) return layer;
  layer = document.createElement('div');
  layer.className = 'bd-threads';
  layer.setAttribute('aria-hidden', 'true');
  const canvas = document.createElement('canvas');
  canvas.className = 'bd-threads-canvas';
  layer.appendChild(canvas);
  stage.prepend(layer);
  return layer;
}

function show() {
  if (failed || shown) return;
  shown = true;
  clearTimeout(sleepTimer);
  const host = ensureLayer();
  if (!threads) {
    threads = new Threads(host, host.firstChild, () => {
      failed = true;
      host.remove();
      layer = null;
      threads = null;
    });
    if (failed) return;
  }
  // the next frame gives the opacity transition a starting point
  requestAnimationFrame(() => {
    if (layer && shown) layer.classList.add('is-on');
  });
  threads.setActive(true);
}

function hide() {
  if (!shown) return;
  shown = false;
  if (layer) layer.classList.remove('is-on');
  clearTimeout(sleepTimer);
  sleepTimer = setTimeout(() => {
    if (!shown && threads) threads.setActive(false);
  }, FADE_MS + 40);
}

if (stage) show();

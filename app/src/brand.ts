/**
 * Roost 品牌素材 —— 内联 SVG
 *
 * 图标用 SVG 绘制（矢量，任意缩放清晰）
 * 与 export-icons.py 的 B2 设计保持一致
 */

export const BRAND = {
  name: "Roost",
  tagline: "ANDROID CONTAINER",
  taglineCN: "安卓容器管理器",
  accent: "#1FA35C",
  accentLight: "#2EE594",
  accentDark: "#18824A",
  ink: "#12161C",
  white: "#FFFFFF",
};

/**
 * 图标：白底圆角方块 + 绿 R + 右下角方块
 *
 * 几何参数（与 Python 生成器对齐，100x100 网格）：
 *   R 字号 60，位置偏移 (-4.8, -3.0)
 *   方块边长 = R 宽度的 33%，间隔 26%
 */
export function roostIcon({
  size = 64,
  bg = "#FFFFFF",
  color = "#1FA35C",
  radius = 22.5,
  mono = false,
} = {}): string {
  // 用 <text> 渲染 R，字体栈优先 Segoe UI Black
  // 这样在浏览器/WebView 里由系统字体渲染，保证字形正确
  const fontStack =
    "'Segoe UI Black','Arial Black','Inter','Noto Sans Black',sans-serif";

  return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 100 100">
  ${
    mono
      ? ""
      : `<rect width="100" height="100" rx="${radius}" fill="${bg}"/>`
  }
  <text x="46" y="70" font-family="${fontStack}" font-size="60" font-weight="900"
        fill="${color}" text-anchor="middle" letter-spacing="-1">R</text>
  <rect x="63.5" y="52" width="18" height="18" rx="4.6" fill="${color}"/>
</svg>`;
}

/**
 * 极简标识（透明底，用于侧边栏嵌入）
 */
export function roostMark({
  size = 24,
  color = "currentColor",
} = {}): string {
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none">
  <text x="10" y="19" font-family="'Segoe UI Black','Arial Black',sans-serif"
        font-size="17" font-weight="900" fill="${color}" text-anchor="middle">R</text>
  <rect x="16.5" y="13.5" width="5.5" height="5.5" rx="1.4" fill="${color}"/>
</svg>`;
}

/**
 * 完整品牌标（横排：图标 + 文字）
 *
 * 布局比例（相对 height）：
 *   图标 100% 高度
 *   间距 28%
 *   主标题 46% 字号，基线在 55%
 *   副标题 14% 字号，基线在 84%
 */
export function roostLogo({
  height = 32,
  dark = true,
} = {}): string {
  const textColor = dark ? "#F0F4F8" : "#12161C";
  const subColor = BRAND.accent;
  const ic = height;                    // 图标边长
  const gap = height * 0.34;            // 图标与文字间距
  const nameSize = height * 0.60;       // 主标题字号（放大）
  const subSize = height * 0.145;       // 副标题字号
  const nameX = ic + gap;
  const subLetter = subSize * 0.18;

  // 估算文字宽度
  const nameW = nameSize * 2.72;
  const subW = subSize * 11.2;
  const width = nameX + Math.max(nameW, subW) + height * 0.18;

  return `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}">
  <g>
    <rect width="${ic}" height="${ic}" rx="${ic * 0.225}" fill="#FFFFFF"/>
    <text x="${ic * 0.46}" y="${ic * 0.71}"
          font-family="'Segoe UI Black','Arial Black',sans-serif"
          font-size="${ic * 0.60}" font-weight="900"
          fill="${BRAND.accent}" text-anchor="middle">R</text>
    <rect x="${ic * 0.635}" y="${ic * 0.52}"
          width="${ic * 0.18}" height="${ic * 0.18}"
          rx="${ic * 0.046}" fill="${BRAND.accent}"/>
  </g>
  <text x="${nameX}" y="${height * 0.56}"
        font-family="'Segoe UI Black','Inter',sans-serif"
        font-size="${nameSize}" font-weight="900"
        fill="${textColor}" letter-spacing="-0.4">Roost</text>
  <text x="${nameX + 0.5}" y="${height * 0.855}"
        font-family="'Segoe UI','Inter',sans-serif"
        font-size="${subSize}" font-weight="700"
        fill="${subColor}" letter-spacing="${subLetter}"
        opacity="0.8">ANDROID CONTAINER</text>
</svg>`;
}

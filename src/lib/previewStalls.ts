import type { PlazaStall } from "../types/ipc";

const featured: PlazaStall[] = [
  {
    endpointId: "preview-0",
    title: "南窗唱片",
    label: "音乐展厅",
    blurb: "夜里的爵士，和一张没放完的民谣",
  },
  {
    endpointId: "preview-1",
    title: "慢船书店",
    label: "小说展厅",
    blurb: "短篇，适合睡前。雨声算在里面",
  },
  {
    endpointId: "preview-2",
    title: "厨房胶片",
    label: "图片展厅",
    blurb: "晚饭前后的光，没有摆拍",
  },
  {
    endpointId: "preview-3",
    title: "二楼放映",
    label: "视频展厅",
    blurb: "自己剪的旅行，可以拖进度",
  },
  {
    endpointId: "preview-4",
    title: "木桌文档",
    label: "文档展厅",
    blurb: "几份还没寄出的说明",
  },
  {
    endpointId: "preview-5",
    title: "阳台电台",
    label: "音乐展厅",
    blurb: "周日的民谣，音量很小",
  },
  {
    endpointId: "preview-6",
    title: "旧磁带",
    label: "音乐展厅",
    blurb: "翻录的现场，有一点底噪",
  },
  {
    endpointId: "preview-7",
    title: "未寄出的信",
    label: "小说展厅",
    blurb: "一个人的章节，看到哪算哪",
  },
];

const names = ["南窗", "慢船", "候车室", "港口", "纸上的海", "邻座", "黑白暗房", "午后乐谱", "旅途", "关门以后", "雨声", "旧城", "二楼", "木桌", "阳台", "未命名"];
const places = ["东口", "夜场", "窗边", "码头", "旧址", "周日", "二店", "廊下"];
const labels = ["音乐展厅", "图片展厅", "视频展厅", "小说展厅", "文档展厅", "原样网站", "本地应用 :3000"];
const lines = [
  "爵士放到一半，茶凉了",
  "胶片扫出来的晚饭",
  "民谣，适合走路听",
  "旅行里剪下来的一段",
  "还没写完的小说",
  "几份可以慢慢翻的文档",
  "一个很小的网站",
  "本机上的页面，转出来看看",
];

let cache: PlazaStall[] | null = null;

export function previewStalls(): PlazaStall[] {
  if (cache) {
    return cache;
  }
  const stalls = [...featured];
  for (let index = featured.length; index < 1000; index += 1) {
    const name = names[index % names.length];
    const place = places[index % places.length];
    const branch = Math.floor(index / names.length) + 1;
    stalls.push({
      endpointId: `preview-${index}`,
      title: branch === 1 ? `${name}·${place}` : `${name}·${place} ${branch}`,
      label: labels[index % labels.length],
      blurb: lines[index % lines.length],
    });
  }
  cache = stalls;
  return stalls;
}

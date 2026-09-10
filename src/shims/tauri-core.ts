export async function invoke(_command: string, _args?: unknown): Promise<never> {
  throw new Error("预览模式：演示数据，不会改动已安装的桌面端。");
}

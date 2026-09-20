/** 与后端同样按 Unicode 字符计数，避免 UTF-16 长度把合法中文/表情密码误判。 */
export const passwordRule = {
  validator: async (_rule: unknown, value: unknown) => {
    if (
      typeof value !== 'string' ||
      [...value].length < 6 ||
      [...value].length > 128 ||
      new TextEncoder().encode(value).length > 512 ||
      !/[a-zA-Z]/.test(value) ||
      !/[0-9]/.test(value)
    ) {
      throw new Error('请输入 6–128 个字符，须包含英文字母和数字');
    }
  },
};

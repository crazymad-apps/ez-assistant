-- Up Migration
-- 首次 SQL 已建表并插入超管；后续失败必须回滚这些内容和版本记录。
SELECT 1/0;

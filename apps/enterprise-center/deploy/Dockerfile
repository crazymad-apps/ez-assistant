FROM node:24-bookworm-slim
WORKDIR /app
ENV NODE_ENV=production CENTER_HOST=0.0.0.0 CENTER_PORT=7320 CENTER_DATABASE_AUTO_UPGRADE=false
# 制品已包含生产依赖、后端和正式后台；镜像构建不安装依赖或编译。
COPY --chown=node:node package.json package-lock.json ./
COPY --chown=node:node node_modules ./node_modules
COPY --chown=node:node dist ./dist
COPY --chown=node:node public ./public
USER node
EXPOSE 7320
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 CMD ["node", "-e", "fetch('http://127.0.0.1:7320/api/info').then(r=>{if(!r.ok)process.exit(1)}).catch(()=>process.exit(1))"]
CMD ["node", "dist/main.js"]

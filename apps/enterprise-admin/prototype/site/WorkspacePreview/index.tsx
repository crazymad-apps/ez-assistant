import { useState } from 'react';
import {
  CheckOutlined,
  DownOutlined,
  FileTextOutlined,
  FolderOutlined,
  PlusOutlined,
  LayoutOutlined,
  SearchOutlined,
  LeftOutlined,
  RightOutlined,
  RobotOutlined,
  ApiOutlined,
  MoreOutlined,
  UserOutlined,
  CopyOutlined,
  BranchesOutlined,
  ReloadOutlined,
  CodeOutlined,
  PictureOutlined,
  SettingOutlined,
  ArrowRightOutlined,
} from '@ant-design/icons';
import { scenarios } from '../content';
import { ResultPreview } from '../ResultPreview';
import logo from '../assets/app-icon.svg';
import styles from './index.module.scss';

/** 官网独立演示投影，布局对齐 Desktop；演示数据不接入真实 Runtime。 */
export function WorkspacePreview() {
  const [selected, setSelected] = useState(0);
  const scenario = scenarios[selected]!;
  const hasImage = scenario.kind === 'drawing' || scenario.kind === 'scene';
  const previewName = scenario.kind === 'scene' ? '三维方案预览.png' : '布局预览.png';

  return (
    <section className={styles.section} aria-label="工作台交互预览">
      <div className={styles.tabs} role="group" aria-label="切换演示场景">
        {scenarios.map((item, index) => (
          <button key={item.id} aria-pressed={selected === index} onClick={() => setSelected(index)}>
            {item.label}
          </button>
        ))}
      </div>
      <div className={styles.window}>
        <div className={styles.workspace}>
          <aside className={styles.sidebar} aria-label="演示会话导航">
            <div className={styles.navigation} aria-hidden="true">
              <img src={logo} alt="" />
              <LayoutOutlined />
              <SearchOutlined />
              <LeftOutlined />
              <RightOutlined />
              <DownOutlined />
            </div>
            <div className={styles.primaryActions}>
              <div>
                <PlusOutlined />
                新建会话
              </div>
              <div>
                <RobotOutlined />
                助理会话
              </div>
              <div>
                <RobotOutlined />
                智能体管理
              </div>
              <div>
                <ApiOutlined />
                插件管理
              </div>
            </div>
            <div className={styles.sideLabel}>
              项目{' '}
              <span>
                <PlusOutlined />
                <DownOutlined />
              </span>
            </div>
            <div className={styles.projectRow}>
              <FolderOutlined />
              库房方案
              <MoreOutlined />
            </div>
            <div className={styles.activeRow}>
              <span>{scenario.title}</span>
              <small>刚刚</small>
            </div>
            <div className={styles.sessionRow}>
              设备维护问答<small>昨天</small>
            </div>
            <div className={styles.sessionRow}>
              项目报价整理<small>昨天</small>
            </div>
            <div className={styles.sideLabel}>
              独立会话{' '}
              <span>
                <PlusOutlined />
                <DownOutlined />
              </span>
            </div>
            <div className={styles.sessionRow}>
              周报技能创建<small>周一</small>
            </div>
            <div className={styles.profile}>
              <UserOutlined />
              <div>
                <strong>演示用户</strong>
                <small>
                  <i />
                  Runtime 已连接
                </small>
              </div>
              <SettingOutlined />
            </div>
          </aside>
          <div className={styles.conversation}>
            <div className={styles.chatHeader}>
              <span>{scenario.title}</span>
              <span className={styles.agentBadge}>工程师助理 · 企业</span>
              <span className={styles.proxy}>
                主控代理 <i />
              </span>
              <span className={styles.headerActions} aria-hidden="true">
                <MoreOutlined />
                <LayoutOutlined />
              </span>
            </div>
            <div className={styles.messages}>
              <div className={styles.userMessage}>{scenario.prompt}</div>
              <div className={styles.messageTime}>14:32</div>
              <details className={styles.process} key={scenario.id}>
                <summary>
                  处理过程 <span>已完成</span>
                  <small>33秒</small>
                </summary>
                <p>{scenario.intro}</p>
                {scenario.steps.map((step) => (
                  <div className={styles.processStep} key={step}>
                    <CheckOutlined />
                    {step}
                  </div>
                ))}
              </details>
              <div className={styles.reply}>
                <p>{scenario.reply}</p>
                <p>
                  已生成文件：
                  <span className={styles.fileLink}>
                    <FileTextOutlined />
                    {scenario.file}
                  </span>
                </p>
                <p>{scenario.body}</p>
                <div className={styles.messageActions} aria-hidden="true">
                  <CopyOutlined />
                  <BranchesOutlined />
                  <time>2026-10-13 14:33:00</time>
                </div>
              </div>
            </div>
            <div className={styles.composer} aria-label="演示输入区">
              <div className={styles.placeholder}>输入消息… / 使用指令</div>
              <div className={styles.composerActions}>
                <PlusOutlined />
                <span>
                  构建 · 自动 <DownOutlined />
                </span>
                <span className={styles.model}>
                  <i />
                  企业模型 / 默认模型 <DownOutlined />
                </span>
                <span className={styles.send} aria-hidden="true">
                  <ArrowRightOutlined rotate={-45} />
                </span>
              </div>
            </div>
          </div>
          <aside className={styles.resources} aria-label="演示资源栏">
            <div className={styles.resourceTabs}>
              <span>
                <FileTextOutlined />
                {scenario.file}
              </span>
              <span className={styles.resourceActive}>
                {hasImage ? <PictureOutlined /> : <FolderOutlined />}
                {hasImage ? previewName : '项目'} <span>×</span>
              </span>
              <PlusOutlined />
            </div>
            {hasImage ? (
              <>
                <div className={styles.breadcrumb}>
                  工具产物 <RightOutlined /> {previewName}
                  <MoreOutlined />
                </div>
                <div className={styles.imagePreview}>
                  <div>
                    <ResultPreview scenario={scenario} />
                  </div>
                </div>
              </>
            ) : (
              <>
                <div className={styles.treeToolbar}>
                  <span>
                    <CheckOutlined />
                    显示生成目录
                  </span>
                  <ReloadOutlined />
                </div>
                <div className={styles.treeRoot}>
                  <DownOutlined />
                  <FolderOutlined />
                  <strong>工作目录</strong>
                </div>
                <div className={styles.directoryName}>库房方案</div>
                <div className={styles.treeRow}>
                  <RightOutlined />
                  <FolderOutlined />
                  参考资料
                </div>
                <div className={styles.treeRow}>
                  <RightOutlined />
                  <FolderOutlined />
                  项目文档
                </div>
                <div className={styles.treeRow}>
                  <DownOutlined />
                  <FolderOutlined />
                  输出成果
                </div>
                <div className={styles.treeFile}>
                  <FileTextOutlined />
                  <span>{scenario.file}</span>
                </div>
                <div className={styles.treeRow}>
                  <CodeOutlined />
                  README.md
                </div>
              </>
            )}
          </aside>
        </div>
      </div>
      <p className={styles.caption}>按客户端布局制作的交互演示 · 内容与文件为示意，可切换场景并展开处理过程。</p>
    </section>
  );
}

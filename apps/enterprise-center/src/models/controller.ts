import { Body, Controller, Delete, Get, HttpCode, Inject, Param, Post, Put, Query, Req } from '@nestjs/common';
import {
  ApiBody,
  ApiCreatedResponse,
  ApiDefaultResponse,
  ApiNoContentResponse,
  ApiOkResponse,
  ApiOperation,
  ApiParam,
  ApiQuery,
  ApiTags,
} from '@nestjs/swagger';
import {
  CatalogSnapshot,
  FixedConfigurationPage,
  ModelConfiguration,
  ModelConfigurationQuery,
  ModelSelection,
  ModelSettings,
  ProviderSummary,
  ProviderUsage,
  RuntimeModelConfiguration,
  SaveModelConfiguration,
  SaveModelSettings,
  SaveProvider,
  TemplateStatus,
} from '../contracts/models.js';
import { CenterErrorResponse } from '../contracts/error.js';
import { Access } from '../security.js';
import type { AuthenticatedRequest } from '../security.js';
import { page, queryFields } from '../validation.js';
import { ModelsService } from './service.js';

@Controller('api')
@Access('admin')
@ApiTags('models')
@ApiDefaultResponse({ type: CenterErrorResponse, description: '身份、参数、配置冲突或服务失败；不返回凭据与上游正文' })
export class ModelsController {
  constructor(@Inject(ModelsService) private readonly models: ModelsService) {}

  @Get('model-providers')
  @ApiOperation({ operationId: 'listModelProviders' })
  @ApiOkResponse({ type: [ProviderSummary] })
  list(@Req() r: AuthenticatedRequest) {
    return this.models.list(r.token);
  }

  @Post('model-providers')
  @ApiOperation({ operationId: 'createModelProvider' })
  @ApiBody({ type: SaveProvider })
  @ApiCreatedResponse({ type: ProviderSummary })
  create(@Body() body: unknown, @Req() r: AuthenticatedRequest) {
    return this.models.save(r.token, undefined, body, r.id);
  }

  @Get('model-providers/:id')
  @ApiOperation({ operationId: 'getModelProvider' })
  @ApiParam({ name: 'id', format: 'uuid' })
  @ApiOkResponse({ type: ProviderSummary })
  get(@Param('id') id: string, @Req() r: AuthenticatedRequest) {
    return this.models.get(r.token, id);
  }

  @Put('model-providers/:id')
  @ApiOperation({ operationId: 'updateModelProvider' })
  @ApiParam({ name: 'id', format: 'uuid' })
  @ApiBody({ type: SaveProvider })
  @ApiOkResponse({ type: ProviderSummary })
  update(@Param('id') id: string, @Body() body: unknown, @Req() r: AuthenticatedRequest) {
    return this.models.save(r.token, id, body, r.id);
  }

  @Get('model-providers/:id/usage')
  @ApiOperation({ operationId: 'getModelProviderUsage' })
  @ApiParam({ name: 'id', format: 'uuid' })
  @ApiOkResponse({ type: ProviderUsage })
  usage(@Param('id') id: string, @Req() r: AuthenticatedRequest) {
    return this.models.usage(r.token, id);
  }

  @Delete('model-providers/:id')
  @ApiOperation({ operationId: 'deleteModelProvider' })
  @ApiParam({ name: 'id', format: 'uuid' })
  @ApiOkResponse({ type: ProviderUsage })
  remove(@Param('id') id: string, @Req() r: AuthenticatedRequest) {
    return this.models.remove(r.token, id, r.id);
  }

  @Get('model-providers/:id/catalog')
  @ApiOperation({ operationId: 'getModelCatalog' })
  @ApiParam({ name: 'id', format: 'uuid' })
  @ApiOkResponse({ type: CatalogSnapshot })
  catalog(@Param('id') id: string, @Req() r: AuthenticatedRequest) {
    return this.models.catalog(r.token, id);
  }

  @Post('model-providers/:id/refresh')
  @HttpCode(200)
  @ApiOperation({ operationId: 'refreshModelCatalog' })
  @ApiBody({ schema: { type: 'object', additionalProperties: false } })
  @ApiParam({ name: 'id', format: 'uuid' })
  @ApiOkResponse({ type: CatalogSnapshot })
  refresh(@Param('id') id: string, @Req() r: AuthenticatedRequest) {
    return this.models.refresh(r.token, id, r.id);
  }

  @Get('model-providers/:id/fixed-configs')
  @ApiOperation({ operationId: 'listFixedModelConfigurations' })
  @ApiParam({ name: 'id', format: 'uuid' })
  @ApiQuery({ name: 'limit', required: false, type: Number })
  @ApiQuery({ name: 'offset', required: false, type: Number })
  @ApiOkResponse({ type: FixedConfigurationPage })
  fixed(@Param('id') id: string, @Query() query: unknown, @Req() r: AuthenticatedRequest) {
    const p = page(queryFields(query, ['limit', 'offset']));
    return this.models.fixedPage(r.token, id, p.limit, p.offset);
  }

  @Post('model-configuration/query')
  @HttpCode(200)
  @ApiOperation({ operationId: 'queryModelConfiguration' })
  @ApiBody({ type: ModelConfigurationQuery })
  @ApiOkResponse({ type: ModelConfiguration })
  configuration(@Body() body: unknown, @Req() r: AuthenticatedRequest) {
    return this.models.query(r.token, body);
  }

  @Put('model-configuration')
  @ApiOperation({ operationId: 'saveModelConfiguration' })
  @ApiBody({ type: SaveModelConfiguration })
  @ApiOkResponse({ type: ModelConfiguration })
  save(@Body() body: unknown, @Req() r: AuthenticatedRequest) {
    return this.models.saveConfiguration(r.token, body, r.id);
  }

  @Post('model-configuration/reset')
  @HttpCode(204)
  @ApiOperation({ operationId: 'resetModelConfiguration' })
  @ApiBody({ type: ModelSelection })
  @ApiNoContentResponse()
  reset(@Body() body: unknown, @Req() r: AuthenticatedRequest) {
    return this.models.reset(r.token, body, r.id);
  }

  @Get('model-settings')
  @ApiOperation({ operationId: 'getModelSettings' })
  @ApiOkResponse({ type: ModelSettings })
  settings(@Req() r: AuthenticatedRequest) {
    return this.models.settings(r.token);
  }

  @Put('model-settings')
  @ApiOperation({ operationId: 'saveModelSettings' })
  @ApiBody({ type: SaveModelSettings })
  @ApiOkResponse({ type: ModelSettings })
  setDefault(@Body() body: unknown, @Req() r: AuthenticatedRequest) {
    return this.models.setDefault(r.token, body, r.id);
  }

  @Get('model-templates')
  @ApiOperation({ operationId: 'getModelTemplateStatus' })
  @ApiOkResponse({ type: TemplateStatus })
  templates() {
    return this.models.templates.status();
  }

  @Post('model-templates/reload')
  @HttpCode(200)
  @ApiOperation({ operationId: 'reloadModelTemplates' })
  @ApiBody({ schema: { type: 'object', additionalProperties: false } })
  @ApiOkResponse({ type: TemplateStatus })
  reload(@Req() r: AuthenticatedRequest) {
    return this.models.reload(r.token, r.id);
  }

  @Get('runtime/model-configuration')
  @Access('authenticated')
  @ApiOperation({ operationId: 'getRuntimeModelConfiguration' })
  @ApiOkResponse({ type: RuntimeModelConfiguration })
  runtime(@Req() r: AuthenticatedRequest) {
    return this.models.runtime(r.token);
  }
}

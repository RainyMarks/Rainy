// @vitest-environment happy-dom
/** IDE failures the Host reports in English reach the user in the interface language when their code is known. */
import { expect, it, vi } from 'vitest'
import { describeIdeError } from '../../src/renderer/app/workbench.ts'
import { IdeRequestError } from '../../src/renderer/ide/ide-api.ts'

vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))

it('words known failure codes in the interface language and keeps details of unknown ones', () => {
  expect(describeIdeError(new IdeRequestError('already-exists', 'The destination already exists.'))).toBe('已存在同名的文件或文件夹。')
  expect(describeIdeError(new IdeRequestError('version-conflict', 'The file changed after it was opened. Reload or compare before saving.')))
    .toBe('磁盘上的文件在打开或预览后已被修改，请比较或刷新后再试。')
  expect(describeIdeError(new IdeRequestError('permission-denied', 'The directory cannot be read: /root'))).toBe('没有权限访问这个位置。')
  expect(describeIdeError(new IdeRequestError('io-error', 'EBUSY: resource busy or locked'))).toBe('EBUSY: resource busy or locked')
  expect(describeIdeError(new Error('plain'))).toBe('plain')
})

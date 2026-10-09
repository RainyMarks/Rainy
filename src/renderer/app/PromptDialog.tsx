/** Generic confirm, text-input and unsaved-changes dialog. */
import { useEffect, useState } from 'react'
import { Button, Modal } from '../ui/index.ts'
import { useIdeT } from '../ide/messages.ts'
import css from './App.module.css'

/** What the dialog asks. */
export interface PromptRequest {
  readonly title: string
  readonly description?: string | undefined
  /** Present for a text question: the initial input value. */
  readonly initial?: string | undefined
  /** Input label; defaults to the workspace path label. */
  readonly label?: string | undefined
  /** Unsaved-changes variant with Save and Discard. */
  readonly dirty?: boolean | undefined
}

/** An open question and the function that answers it. */
export interface PendingPrompt extends PromptRequest {
  readonly resolve: (value: string | null) => void
}

/**
 * Render the open question.
 * @param props.prompt Open question, or undefined while none is open.
 * @param props.close Answers it: `null` for cancel, the input text, `'confirm'`, `'save'` or `'discard'`.
 * @returns The dialog.
 */
export function PromptDialog({ prompt, close }: { prompt: PendingPrompt | undefined; close: (result: string | null) => void }) {
  const t = useIdeT()
  const [value, setValue] = useState('')
  useEffect(() => { setValue(prompt?.initial ?? '') }, [prompt])
  const confirm = (): void => { close(prompt?.dirty === true ? 'save' : prompt?.initial === undefined ? 'confirm' : value) }
  return (
    <Modal
      open={prompt !== undefined}
      className={css.promptDialog}
      contentClassName={css.dialogContent}
      onClose={() => { close(null) }}
      title={prompt?.title ?? ''}
      description={prompt?.description ?? ''}
      closeLabel={t('ideClose')}
      footer={<>
        <Button onClick={() => { close(null) }}>{t('ideCancel')}</Button>
        {prompt?.dirty === true && <Button onClick={() => { close('discard') }}>{t('ideDiscard')}</Button>}
        <Button onClick={confirm}>{t(prompt?.dirty === true ? 'ideSave' : 'ideConfirm')}</Button>
      </>}
    >
      {prompt?.initial !== undefined && (
        <label className={css.field}>
          {prompt.label ?? t('idePath')}
          <input className={css.input} data-modal-autofocus value={value}
            onChange={(event) => { setValue(event.target.value) }}
            onKeyDown={(event) => { if (event.key === 'Enter' && !event.nativeEvent.isComposing) close(value) }} />
        </label>
      )}
    </Modal>
  )
}

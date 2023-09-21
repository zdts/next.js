import React, { Suspense } from 'react'
import Link from 'next/link'

import { Dynamic } from './dynamic'

export function Page({ useSuspense = false }) {
  let slot = <Dynamic />
  if (useSuspense) {
    slot = (
      <Suspense
        fallback={
          <div
            style={{
              padding: '10px',
              flexGrow: '1',
            }}
          >
            Loading...
          </div>
        }
      >
        {slot}
      </Suspense>
    )
  }

  return (
    <div>
      <div id="page" style={{ padding: '10px' }}>
        Page
      </div>
      <div
        style={{
          border: '1px solid black',
          display: 'flex',
          textAlign: 'center',
        }}
      >
        <div
          style={{
            padding: '10px',
            fontWeight: 'bold',
            color: 'white',
            background: 'black',
          }}
        >
          Dynamic
        </div>
        {slot}
      </div>
      <div style={{ padding: '10px' }}>
        <div>
          <Link href="/">/ - Home</Link>
        </div>
      </div>
    </div>
  )
}

export function NonSuspensePage() {
  return <Page />
}

export function SuspensePage() {
  return <Page useSuspense={true} />
}

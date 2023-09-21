import React from 'react'
import { cookies } from 'next/headers'

export function Dynamic() {
  // const message = 'Disabled'
  const message = cookies().has('session') ? 'Signed In' : 'Signed Out'

  return (
    <div
      id="dynamic"
      style={{
        backgroundColor: 'rgb(0, 112, 243)',
        color: 'white',
        padding: '10px',
        flexGrow: '1',
      }}
    >
      {message}
    </div>
  )
}
